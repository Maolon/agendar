#![allow(clippy::result_large_err)]

//! App backend tests: temp Vault commands, the rubber-stamp guard, stale
//! prepare tokens and the notification dispatcher with a fake transport.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tempfile::TempDir;

use agendar_app::guard;
use agendar_app::host::{Host, NoticeAction, NoticeOutcome, Origin, WallClock};
use agendar_app::notifications::{
    Availability, NoticeSink, NoticeSpec, approve_eligible, notice_eligible,
};
use agendar_app::preferences::Preferences;
use agendar_app::ui::Ui;
use agendar_core::model::{
    Agent, BlobExt, BlobRef, Claim, CliTranscriptDemo, CriterionResult, DecisionCard, Delivery,
    Demo, DemoCheck, Disclosure, EventId, Harness, Mode, NeedsKind, NeedsYouItem, NodePath,
    ProjectSlug, ScreenshotDemo, UtcTime,
};
use agendar_core::runtime::Config;
use agendar_core::runtime::checks::{self as check_runtime, CheckProfile};
use agendar_core::runtime::steward::{SessionRegistration, register_session};
use agendar_core::service::Service;
use agendar_core::vault::blobs::put_blob;

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

struct ManualClock {
    now: Mutex<UtcTime>,
}

impl ManualClock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(UtcTime::parse("2026-01-01T00:00:00.000Z").expect("fixed time")),
        })
    }

    fn advance(&self, seconds: i64) {
        let mut now = self.now.lock().expect("clock");
        *now = now.add_millis(seconds * 1_000);
    }
}

impl WallClock for ManualClock {
    fn now(&self) -> UtcTime {
        *self.now.lock().expect("clock")
    }
}

#[derive(Default)]
struct FakeUi {
    shown_main: AtomicUsize,
    hidden: AtomicUsize,
    focus: Mutex<Vec<Value>>,
    changed: Mutex<Vec<Value>>,
    unblock: Mutex<Vec<Value>>,
    started: AtomicUsize,
    attention: Mutex<Vec<usize>>,
    working: Mutex<Vec<usize>>,
}

impl Ui for FakeUi {
    fn show_main(&self) {
        self.shown_main.fetch_add(1, Ordering::SeqCst);
    }

    fn hide_popover(&self) {
        self.hidden.fetch_add(1, Ordering::SeqCst);
    }

    fn focus(&self, focus: &Value) {
        self.focus.lock().expect("focus").push(focus.clone());
    }

    fn emit_changed(&self, event: &Value) {
        self.changed.lock().expect("changed").push(event.clone());
    }

    fn emit_unblock(&self, event: &Value) {
        self.unblock.lock().expect("unblock").push(event.clone());
    }

    fn emit_health(&self, _event: &Value) {}

    fn start_notifications(&self) {
        self.started.fetch_add(1, Ordering::SeqCst);
    }

    fn attention(&self, needs: usize, working: usize) {
        self.attention.lock().expect("attention").push(needs);
        self.working.lock().expect("working").push(working);
    }
}

#[derive(Default)]
struct FakeSink {
    shown: Mutex<Vec<NoticeSpec>>,
    withdrawn: Mutex<Vec<String>>,
    last_error: Mutex<Option<String>>,
    refreshed: AtomicUsize,
    fail: AtomicBool,
    available: AtomicBool,
}

impl FakeSink {
    fn available() -> Arc<Self> {
        Arc::new(Self {
            available: AtomicBool::new(true),
            ..Self::default()
        })
    }

    fn withdrawn(&self) -> Vec<String> {
        self.withdrawn.lock().expect("withdrawn").clone()
    }

    fn shown(&self) -> Vec<NoticeSpec> {
        self.shown.lock().expect("shown").clone()
    }
}

impl NoticeSink for FakeSink {
    fn show(&self, notice: &NoticeSpec) -> Result<(), String> {
        if self.fail.load(Ordering::SeqCst) {
            return Err("fake transport refused".to_owned());
        }
        self.shown.lock().expect("shown").push(notice.clone());
        Ok(())
    }

    fn withdraw(&self, id: &str) -> Result<(), String> {
        self.withdrawn
            .lock()
            .expect("withdrawn")
            .push(id.to_owned());
        Ok(())
    }

    fn last_error(&self) -> Option<String> {
        self.last_error.lock().expect("last error").clone()
    }

    fn refresh(&self) {
        self.refreshed.fetch_add(1, Ordering::SeqCst);
    }

    fn capability(&self) -> Availability {
        if self.available.load(Ordering::SeqCst) {
            Availability::Available
        } else {
            Availability::Unavailable
        }
    }

    fn reason(&self) -> Option<String> {
        if self.available.load(Ordering::SeqCst) {
            None
        } else {
            Some("notification permission was denied".to_owned())
        }
    }
}

// ---------------------------------------------------------------------------
// Scratch Vault fixtures
// ---------------------------------------------------------------------------

struct Scratch {
    _root: TempDir,
    data: PathBuf,
    vault: PathBuf,
    project: ProjectSlug,
    asks: Vec<(EventId, String)>,
}

fn card(reversibility: &str, confidence: &str, external: bool, why_now: &str) -> DecisionCard {
    let value = json!({
        "why_now": why_now,
        "question": "How should answers reach idle agents?",
        "reversibility": reversibility,
        "reach": "local",
        "external": external,
        "domain": "arch",
        "options": [
            {
                "id": "adapters",
                "label": "Use harness adapters",
                "consequence": "Use hooks for the current harness.",
                "reversibility": reversibility,
                "external": external,
                "eta": {"min_s": 1800, "max_s": 3600},
                "icon": "link"
            },
            {
                "id": "inbox",
                "label": "Use inbox on next start",
                "consequence": "Deliver on the next explicit inbox read.",
                "reversibility": reversibility,
                "external": external,
                "eta": {"min_s": 900, "max_s": 1800},
                "icon": "inbox"
            }
        ],
        "recommended": {
            "option": "adapters",
            "confidence": confidence,
            "because": "It preserves the original session when the spike succeeds."
        },
        "already_checked": [],
        "done_when": [
            {"id": "continues", "text": "The agent reads the receipt and acknowledges."}
        ],
        "on_timeout": {"kind": "hold"}
    });
    serde_json::from_value(value).expect("valid card")
}

/// Build one scratch Vault with one feature and one ask per card.
fn scratch(cards: Vec<(&str, DecisionCard, bool)>) -> Scratch {
    let root = tempfile::tempdir().expect("root");
    let vault = root.path().join("vault");
    let data = root.path().join("data");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    let (service, _) = Service::init(&vault, &data).expect("init");
    let project = ProjectSlug::parse("demo").expect("project");
    service
        .select_config(Some(&project))
        .expect("select config");
    Config::map_repo(&data, &repo, &project).expect("link repo");
    let (pid, process_start) =
        agendar_core::runtime::sessions::process_identity(std::process::id())
            .map(|start| (std::process::id(), start))
            .expect("test process identity");
    let session = register_session(
        &service,
        SessionRegistration {
            project: project.clone(),
            harness: Harness::Generic,
            harness_session_id: "app-test".to_owned(),
            pid,
            process_start,
            repo_root: repo.clone(),
        },
    )
    .expect("register session");
    use agendar_core::model::{DemoKind, PlanFeature, ProjectDeclared};
    service
        .declare_project(
            &project,
            &session,
            ProjectDeclared {
                agent: None,
                title: Some("Demo".to_owned()),
                brief: Some("Scratch board.".to_owned()),
                goal: Some("Prove the app backend.".to_owned()),
                glyph: Some("radar".to_owned()),
                archived: None,
                icon: None,
            },
            None,
        )
        .expect("declare project");
    let mut asks = Vec::new();
    for (node_name, card, started) in cards {
        let node = NodePath::parse(node_name).expect("node");
        service
            .declare_feature(
                &project,
                &session,
                node.clone(),
                PlanFeature {
                    agent: None,
                    title: Some(format!("Feature {node_name}")),
                    depends_on: None,
                    slice: Some(node_name.to_owned()),
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
        if started {
            service
                .progress_started(
                    &project,
                    &session,
                    &node,
                    agendar_core::model::EtaRange {
                        min_s: agendar_core::model::Seconds::new(600).expect("seconds"),
                        max_s: agendar_core::model::Seconds::new(1200).expect("seconds"),
                    },
                    None,
                    None,
                )
                .expect("start work");
        }
        let result = service
            .ask(&project, &session, &node, card, Mode::Ask, None)
            .expect("ask");
        asks.push((result.ask_id.clone(), node.as_str().to_owned()));
    }
    drop(service);
    Scratch {
        _root: root,
        data,
        vault,
        project,
        asks,
    }
}

impl Scratch {
    fn host(&self, clock: Arc<ManualClock>) -> (Arc<Host>, Arc<FakeUi>, Arc<FakeSink>) {
        let ui = Arc::new(FakeUi::default());
        let sink = FakeSink::available();
        let host = Arc::new(Host::new(&self.data, clock, ui.clone(), sink.clone()).expect("host"));
        (host, ui, sink)
    }

    fn service(&self) -> Service {
        Service::open(&self.vault, &self.data).expect("service")
    }

    fn answer_count(&self, host: &Host) -> usize {
        host.event_lines()
            .iter()
            .filter(|line| line.contains("\"kind\":\"decision.answer\""))
            .count()
    }

    fn snapshot(&self, host: &Host) -> Value {
        host.get_snapshot().expect("snapshot")
    }

    fn decision<'a>(&self, snapshot: &'a Value, ask: &EventId) -> &'a Value {
        &snapshot["state"]["projects"][self.project.as_str()]["decisions"][ask.as_str()]
    }

    fn need_id(&self, snapshot: &Value, ask: &EventId) -> String {
        snapshot["needs"]
            .as_array()
            .expect("needs")
            .iter()
            .find(|need| need["source"].as_str() == Some(ask.as_str()))
            .and_then(|need| need["id"].as_str())
            .expect("need for ask")
            .to_owned()
    }

    fn enable_notifications(&self, host: &Host, snapshot: &Value) {
        let revision = snapshot["preferences"]["revision"]
            .as_str()
            .expect("preferences revision");
        host.set_preferences(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "expected_revision": revision,
            "notifications": true,
        }))
        .expect("enable notifications");
    }

    fn prepare(&self, host: &Host, ask: &EventId, option: &str, revision: &str) -> Value {
        host.prepare_answer(
            "main",
            &json!({
                "ask_id": ask.as_str(),
                "option_id": option,
                "expected_decision_revision": revision,
            }),
        )
        .expect("prepare")
    }

    fn answer(
        &self,
        host: &Host,
        token: &str,
        label: &str,
    ) -> Result<Value, agendar_app::error::AppError> {
        host.answer_decision(
            "main",
            &json!({
                "request_id": ulid::Ulid::generate().to_string(),
                "token": token,
                "echo_label": label,
            }),
        )
    }
}

fn revision(snapshot: &Value, project: &str, ask: &EventId) -> String {
    snapshot["state"]["projects"][project]["decisions"][ask.as_str()]["revision"]
        .as_str()
        .expect("decision revision")
        .to_owned()
}

// ---------------------------------------------------------------------------
// Commands against a temp Vault
// ---------------------------------------------------------------------------

#[test]
fn an_unconfigured_host_reports_the_setup_revision_tokens() {
    let root = tempfile::tempdir().expect("root");
    let ui = Arc::new(FakeUi::default());
    let sink = FakeSink::available();
    let host = Host::new(root.path(), ManualClock::new(), ui, sink).expect("host");
    let error = host.get_snapshot().expect_err("unconfigured");
    assert_eq!(error.code, "E_CONTEXT");
    let details = error.details.expect("setup details");
    let fields: Vec<&str> = details.iter().map(|detail| detail.field.as_str()).collect();
    assert!(fields.contains(&"expected_config_revision"));
    assert!(fields.contains(&"expected_preferences_revision"));
    assert!(details.iter().all(|detail| detail.message.len() == 64));
}

#[test]
fn commands_read_and_answer_a_real_temp_vault() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let clock = ManualClock::new();
    let (host, ui, _sink) = fixture.host(clock);
    let snapshot = fixture.snapshot(&host);
    let (ask, _) = fixture.asks[0].clone();
    let need_id = fixture.need_id(&snapshot, &ask);
    assert!(!need_id.is_empty(), "the ask is an attention row");
    let rev = revision(&snapshot, "demo", &ask);
    assert_eq!(rev.len(), 64);

    let prepared = fixture.prepare(&host, &ask, "adapters", &rev);
    assert_eq!(prepared["guard"], "easy");
    assert_eq!(prepared["label"], "Use harness adapters");
    assert_eq!(prepared["receipt"]["preview"], true);
    let token = prepared["token"].as_str().expect("token").to_owned();
    let label = prepared["label"].as_str().expect("label").to_owned();

    let answered = fixture.answer(&host, &token, &label).expect("answer");
    assert_eq!(answered["receipt"]["chose"]["label"], label);
    assert_eq!(answered["awaiting_ack"], true);
    assert_eq!(fixture.answer_count(&host), 1);
    assert!(!ui.changed.lock().expect("changed").is_empty());

    let after = fixture.snapshot(&host);
    assert_eq!(
        fixture.decision(&after, &ask)["lifecycle"].as_str(),
        Some("answered")
    );
    assert!(
        after["needs"]
            .as_array()
            .expect("needs")
            .iter()
            .all(|need| need["source"].as_str() != Some(ask.as_str())),
        "the answered decision leaves the needs queue"
    );
}

#[test]
fn an_identical_request_retry_replays_the_committed_result() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let snapshot = fixture.snapshot(&host);
    let (ask, _) = fixture.asks[0].clone();
    let rev = revision(&snapshot, "demo", &ask);
    let prepared = fixture.prepare(&host, &ask, "adapters", &rev);
    let token = prepared["token"].as_str().expect("token").to_owned();
    let label = prepared["label"].as_str().expect("label").to_owned();
    let request_id = ulid::Ulid::generate().to_string();
    let request = json!({
        "request_id": request_id,
        "token": token,
        "echo_label": label,
    });
    let first = host.answer_decision("main", &request).expect("answer");
    let second = host.answer_decision("main", &request).expect("retry");
    assert_eq!(first["event_id"], second["event_id"]);
    assert_eq!(fixture.answer_count(&host), 1, "one event for the retry");
}

// ---------------------------------------------------------------------------
// Rubber-stamp guard and stale tokens
// ---------------------------------------------------------------------------

#[test]
fn the_backend_refuses_an_answer_that_does_not_match_the_guard() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let snapshot = fixture.snapshot(&host);
    let (ask, _) = fixture.asks[0].clone();
    let rev = revision(&snapshot, "demo", &ask);

    // Easy risk but a non-recommended option: a human who deliberately picks
    // the other option from the main window answers the real question. Only
    // the shortcut (recommendation) is recommendation-only.
    let prepared = fixture.prepare(&host, &ask, "inbox", &rev);
    assert_eq!(prepared["guard"], "easy");
    let token = prepared["token"].as_str().expect("token").to_owned();
    let answered = fixture
        .answer(&host, &token, "Use inbox on next start")
        .expect("an explicit main-window alternative commits");
    assert_eq!(
        answered["receipt"]["chose"]["label"],
        "Use inbox on next start"
    );
    assert_eq!(fixture.answer_count(&host), 1);

    // The popover (and a notification) only ever offer the current easy
    // recommendation; the alternative is refused before a token exists.
    let assistant = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let (assistant_host, _ui, _sink) = assistant.host(ManualClock::new());
    let assistant_snapshot = assistant.snapshot(&assistant_host);
    let (assistant_ask, _) = assistant.asks[0].clone();
    let assistant_rev = revision(&assistant_snapshot, "demo", &assistant_ask);
    let popover_error = assistant_host
        .prepare_answer(
            "popover",
            &json!({
                "ask_id": assistant_ask.as_str(),
                "option_id": "inbox",
                "expected_decision_revision": assistant_rev,
            }),
        )
        .expect_err("the popover may not prepare an alternative");
    assert_eq!(popover_error.code, "E_CONSENT");
    assert_eq!(assistant.answer_count(&assistant_host), 0);

    // A costly card needs the echoed chosen label; a mismatched echo writes
    // nothing and the exact echo is accepted.
    let costly = scratch(vec![(
        "core/wake",
        card("costly", "med", false, "Wake is blocked."),
        false,
    )]);
    let (costly_host, _ui, _sink) = costly.host(ManualClock::new());
    let costly_snapshot = costly.snapshot(&costly_host);
    let (costly_ask, _) = costly.asks[0].clone();
    let costly_rev = revision(&costly_snapshot, "demo", &costly_ask);
    let popover_error = costly_host
        .prepare_answer(
            "popover",
            &json!({
                "ask_id": costly_ask.as_str(),
                "option_id": "adapters",
                "expected_decision_revision": costly_rev,
            }),
        )
        .expect_err("the popover may not prepare a costly decision");
    assert_eq!(popover_error.code, "E_CONSENT");
    let prepared = costly.prepare(&costly_host, &costly_ask, "adapters", &costly_rev);
    assert_eq!(prepared["guard"], "choose");
    let token = prepared["token"].as_str().expect("token").to_owned();
    let error = costly
        .answer(&costly_host, &token, "something else")
        .expect_err("the echo must match");
    assert_eq!(error.code, "E_VALIDATION");
    assert_eq!(costly.answer_count(&costly_host), 0);
    costly
        .answer(&costly_host, &token, "Use harness adapters")
        .expect("the exact echo is accepted");
    assert_eq!(costly.answer_count(&costly_host), 1);
}

#[test]
fn a_prepare_token_expires_after_sixty_seconds() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let clock = ManualClock::new();
    let (host, _ui, _sink) = fixture.host(Arc::clone(&clock));
    let snapshot = fixture.snapshot(&host);
    let (ask, _) = fixture.asks[0].clone();
    let rev = revision(&snapshot, "demo", &ask);
    let prepared = fixture.prepare(&host, &ask, "adapters", &rev);
    let token = prepared["token"].as_str().expect("token").to_owned();
    clock.advance(61);
    let error = fixture
        .answer(&host, &token, "Use harness adapters")
        .expect_err("expired token");
    assert_eq!(error.code, "E_STALE_ANSWER");
    assert_eq!(fixture.answer_count(&host), 0);
}

#[test]
fn a_changed_decision_rejects_the_stale_prepare_token() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let snapshot = fixture.snapshot(&host);
    let (ask, _) = fixture.asks[0].clone();
    let rev = revision(&snapshot, "demo", &ask);
    let prepared = fixture.prepare(&host, &ask, "adapters", &rev);
    let token = prepared["token"].as_str().expect("token").to_owned();

    // A direct operator answer through the shared service changes the revision.
    let service = fixture.service();
    let project = ProjectSlug::parse("demo").expect("project");
    let option = agendar_core::model::OptionId::parse("adapters").expect("option");
    service
        .answer_decision(&project, &ask, &option, None, None, None)
        .expect("direct answer");
    drop(service);

    let error = fixture
        .answer(&host, &token, "Use harness adapters")
        .expect_err("stale token");
    assert_eq!(error.code, "E_STALE_ANSWER");
    assert_eq!(
        fixture.answer_count(&host),
        1,
        "only the direct answer exists"
    );
}

#[test]
fn prepare_rejects_a_wrong_expected_revision() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let (ask, _) = fixture.asks[0].clone();
    let error = host
        .prepare_answer(
            "main",
            &json!({
                "ask_id": ask.as_str(),
                "option_id": "adapters",
                "expected_decision_revision": "0".repeat(64),
            }),
        )
        .expect_err("stale revision");
    assert_eq!(error.code, "E_STALE_ANSWER");
}

// ---------------------------------------------------------------------------
// Notification eligibility, routing and dedupe
// ---------------------------------------------------------------------------

fn synthetic_need(ask: &EventId, kind: NeedsKind, stalled: u32) -> NeedsYouItem {
    NeedsYouItem {
        id: format!("need-{}", ask.as_str()),
        kind,
        project: ProjectSlug::parse("demo").expect("project"),
        node: Some(NodePath::parse("core/wake").expect("node")),
        source: ask.clone(),
        question: "How should answers reach idle agents?".to_owned(),
        since: UtcTime::parse("2026-01-01T00:00:00.000Z").expect("time"),
        stalled_agents: stalled,
        waiting_minutes: 5,
        weight_x2: 4,
        score_x2: 20,
        reason: "blocked".to_owned(),
        reason_msg: None,
        question_msg: None,
        unblocks: Vec::new(),
    }
}

#[test]
fn the_approve_eligibility_matrix() {
    let fixture = scratch(vec![
        (
            "core/easy",
            card("easy", "high", false, "Wake is blocked."),
            false,
        ),
        (
            "core/low",
            card("easy", "low", false, "Wake is blocked."),
            false,
        ),
        (
            "core/costly",
            card("costly", "high", false, "Wake is blocked."),
            false,
        ),
        (
            "core/irrev",
            card("irreversible", "high", false, "Wake is blocked."),
            false,
        ),
        (
            "core/ext",
            card("easy", "high", true, "Wake is blocked."),
            false,
        ),
        (
            "core/heuristic",
            card("easy", "high", false, "Deploy the migration tonight."),
            false,
        ),
    ]);
    let service = fixture.service();
    let now = UtcTime::parse("2026-01-01T00:00:00.000Z").expect("time");
    let fresh = service.refresh(now, false).expect("refresh");
    let project = &fresh.state.projects[&fixture.project];
    let decision = |node: &str| {
        let feature = &project.features[&NodePath::parse(node).expect("node")];
        let _ = feature;
        // Decisions are keyed by ask id; find the one whose node matches.
        project
            .decisions
            .values()
            .find(|decision| decision.node.as_str() == node)
            .expect("decision")
    };
    let allowed = |node: &str, confidence: &str| {
        let decision = decision(node);
        let mut need = synthetic_need(&decision.id, NeedsKind::Decide, 1);
        need.node = Some(decision.node.clone());
        let eligible = approve_eligible(&need, Some(decision));
        if confidence == "none" {
            assert!(eligible.is_none(), "{node} must not offer approve");
        } else {
            let (option, label) = eligible.unwrap_or_else(|| panic!("{node} should offer approve"));
            assert_eq!(option.as_str(), "adapters");
            assert_eq!(label, "Use harness adapters");
        }
    };
    allowed("core/easy", "high");
    allowed("core/low", "none");
    allowed("core/costly", "none");
    allowed("core/irrev", "none");
    allowed("core/ext", "none");
    allowed("core/heuristic", "none");

    let easy = decision("core/easy");
    let mut answered = easy.clone();
    answered.lifecycle = agendar_core::model::DecisionLifecycle::Answered;
    let mut need = synthetic_need(&easy.id, NeedsKind::Decide, 1);
    need.node = Some(easy.node.clone());
    assert!(approve_eligible(&need, Some(&answered)).is_none());
    // An open easy question is announced, and may be approved, even while its
    // agent still has other work: being stalled is not a safety fact.
    let unstalled = synthetic_need(&easy.id, NeedsKind::Decide, 0);
    assert!(notice_eligible(&unstalled));
    assert!(approve_eligible(&unstalled, Some(easy)).is_some());
    assert!(
        approve_eligible(&synthetic_need(&easy.id, NeedsKind::Confirm, 1), Some(easy)).is_none()
    );
    let rescue = synthetic_need(&easy.id, NeedsKind::Rescue, 0);
    assert!(notice_eligible(&rescue));
    assert!(approve_eligible(&rescue, None).is_none());
}

#[test]
fn notifications_offer_approve_only_for_eligible_items_and_answer_once() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let clock = ManualClock::new();
    let (host, ui, sink) = fixture.host(Arc::clone(&clock));
    let snapshot = fixture.snapshot(&host);
    fixture.enable_notifications(&host, &snapshot);
    fixture.snapshot(&host);
    let notices = sink.shown();
    assert_eq!(notices.len(), 1, "one stalled decide item is announced");
    assert_eq!(
        notices[0].choices.first().map(String::as_str),
        Some("Use harness adapters"),
        "the recommended option leads, labelled with its own text"
    );
    assert!(ui.started.load(Ordering::SeqCst) >= 1);

    let notice = &notices[0];
    let outcome = host.handle_notice(&notice.id, NoticeAction::Choose(0), Origin::Live);
    assert_eq!(outcome, NoticeOutcome::Answered { duplicate: false });
    assert_eq!(fixture.answer_count(&host), 1);
    let repeated = host.handle_notice(&notice.id, NoticeAction::Choose(0), Origin::Live);
    assert_eq!(repeated, NoticeOutcome::Answered { duplicate: true });
    assert_eq!(
        fixture.answer_count(&host),
        1,
        "no duplicate operator event"
    );
}

#[test]
fn an_expired_notification_approval_only_routes_and_writes_nothing() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let clock = ManualClock::new();
    let (host, ui, sink) = fixture.host(Arc::clone(&clock));
    let snapshot = fixture.snapshot(&host);
    fixture.enable_notifications(&host, &snapshot);
    let snapshot = fixture.snapshot(&host);
    let ask = fixture.asks[0].0.clone();
    let need_id = fixture.need_id(&snapshot, &ask);
    let notice = sink.shown().into_iter().next().expect("notice");
    clock.advance(61);
    let outcome = host.handle_notice(&notice.id, NoticeAction::Choose(0), Origin::Live);
    assert_eq!(
        outcome,
        NoticeOutcome::Routed {
            item_id: need_id.clone(),
            status: "review_current".to_owned()
        }
    );
    assert_eq!(fixture.answer_count(&host), 0);
    let focus = ui
        .focus
        .lock()
        .expect("focus")
        .last()
        .cloned()
        .expect("focus");
    assert_eq!(focus["item_id"], need_id);
}

#[test]
fn open_focuses_the_exact_item_not_the_top_row() {
    let fixture = scratch(vec![
        (
            "core/one",
            card("easy", "med", false, "Wake is blocked."),
            true,
        ),
        (
            "core/two",
            card("costly", "med", false, "Wake is blocked."),
            true,
        ),
    ]);
    let (host, ui, sink) = fixture.host(ManualClock::new());
    let snapshot = fixture.snapshot(&host);
    fixture.enable_notifications(&host, &snapshot);
    let snapshot = fixture.snapshot(&host);
    let needs = snapshot["needs"].as_array().expect("needs").to_vec();
    assert!(needs.len() >= 2);
    let target = needs.last().expect("target").clone();
    let target_id = target["id"].as_str().expect("id").to_owned();
    let target_source = target["source"].as_str().expect("source").to_owned();
    let notice = sink
        .shown()
        .into_iter()
        .find(|notice| notice.item_id == target_id)
        .expect("notice for the target");
    assert!(notice.choices.is_empty(), "costly items get Open only");
    let outcome = host.handle_notice(&notice.id, NoticeAction::Open, Origin::Live);
    assert_eq!(
        outcome,
        NoticeOutcome::Focused {
            item_id: target_id.clone(),
            sequence: 1
        }
    );
    let focus = ui
        .focus
        .lock()
        .expect("focus")
        .last()
        .cloned()
        .expect("focus");
    assert_eq!(focus["item_id"], target_id);
    assert_eq!(focus["project"], "demo");
    let source_ask = fixture
        .asks
        .iter()
        .find(|(ask, _)| ask.as_str() == target_source)
        .map(|(ask, _)| ask.clone())
        .expect("ask for target");
    assert_eq!(focus["item_id"], fixture.need_id(&snapshot, &source_ask));
}

#[test]
fn two_open_clicks_keep_a_monotonic_focus_sequence() {
    let fixture = scratch(vec![(
        "core/one",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let (host, ui, sink) = fixture.host(ManualClock::new());
    let snapshot = fixture.snapshot(&host);
    fixture.enable_notifications(&host, &snapshot);
    fixture.snapshot(&host);
    let notice = sink.shown().into_iter().next().expect("notice");
    assert_eq!(
        host.handle_notice(&notice.id, NoticeAction::Open, Origin::Live),
        NoticeOutcome::Focused {
            item_id: notice.item_id.clone(),
            sequence: 1
        }
    );
    assert_eq!(
        host.handle_notice(&notice.id, NoticeAction::Open, Origin::Live),
        NoticeOutcome::Focused {
            item_id: notice.item_id.clone(),
            sequence: 2
        }
    );
    let focus = ui.focus.lock().expect("focus").clone();
    assert_eq!(focus.len(), 2);
    assert_eq!(focus[0]["sequence"], 1);
    assert_eq!(focus[1]["sequence"], 2);
    assert_eq!(focus[1]["item_id"], notice.item_id);
    let after = fixture.snapshot(&host);
    assert_eq!(after["notification_focus"]["sequence"], 2);
    assert_eq!(after["notification_focus"]["item_id"], notice.item_id);
}

#[test]
fn a_failed_transport_does_not_pretend_the_notice_was_shown() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let (host, _ui, sink) = fixture.host(ManualClock::new());
    let snapshot = fixture.snapshot(&host);
    fixture.enable_notifications(&host, &snapshot);
    sink.fail.store(true, Ordering::SeqCst);
    fixture.snapshot(&host);
    assert!(sink.shown().is_empty());
    sink.fail.store(false, Ordering::SeqCst);
    fixture.snapshot(&host);
    assert_eq!(
        sink.shown().len(),
        1,
        "the notice is retried after recovery"
    );
}

#[test]
fn the_menu_bar_glyph_ignores_snoozed_items() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let (host, ui, _sink) = fixture.host(ManualClock::new());
    let snapshot = fixture.snapshot(&host);
    let item = snapshot["needs"][0]["id"]
        .as_str()
        .expect("need")
        .to_owned();
    let waiting = snapshot["needs"].as_array().expect("needs").len();
    assert_eq!(
        ui.attention.lock().expect("attention").last(),
        Some(&waiting),
        "the glyph counts what is waiting"
    );
    host.snooze_need(&json!({
        "request_id": ulid::Ulid::generate().to_string(),
        "item_id": item,
        "until": "2026-01-01T01:00:00.000Z",
    }))
    .expect("snooze");
    assert_eq!(
        ui.attention.lock().expect("attention").last(),
        Some(&(waiting - 1)),
        "a snoozed item no longer turns the glyph amber"
    );
    host.snooze_need(&json!({
        "request_id": ulid::Ulid::generate().to_string(),
        "item_id": item,
        "until": "2026-01-01T00:00:00.000Z",
    }))
    .expect("restore");
    assert_eq!(
        ui.attention.lock().expect("attention").last(),
        Some(&waiting)
    );
}

#[test]
fn snooze_need_patches_one_entry_and_only_for_a_waiting_item() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let snapshot = fixture.snapshot(&host);
    let item = snapshot["needs"][0]["id"]
        .as_str()
        .expect("need")
        .to_owned();
    let snoozed = host
        .snooze_need(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "item_id": item,
            "until": "2026-01-01T01:00:00.000Z",
        }))
        .expect("no preferences revision is needed");
    assert_eq!(
        snoozed["preferences"]["snoozed"][&item],
        "2026-01-01T01:00:00.000Z"
    );
    let unknown = host
        .snooze_need(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "item_id": "decide:not-waiting",
            "until": "2026-01-01T01:00:00.000Z",
        }))
        .expect_err("only an item waiting now can be snoozed");
    assert_eq!(unknown.code, "E_NOT_FOUND");
    let restored = host
        .snooze_need(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "item_id": item,
            "until": "2026-01-01T00:00:00.000Z",
        }))
        .expect("restore");
    assert!(restored["preferences"]["snoozed"].get(&item).is_none());
}

#[test]
fn settings_sees_which_agent_kinds_reported_from_this_computer() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let snapshot = fixture.snapshot(&host);
    let session = snapshot["state"]["projects"]["demo"]["sessions"]
        .as_object()
        .expect("sessions")
        .values()
        .next()
        .expect("one session")
        .clone();
    let seen = host.last_seen_by_harness();
    let harness = session["harness"].as_str().expect("harness");
    assert_eq!(
        seen.get(harness).map(|ts| ts.canonical()),
        session["last_event_ts"].as_str().map(str::to_owned),
        "the session's newest event is the kind's last report"
    );
}

#[test]
fn a_snooze_is_stored_locally_and_a_restore_removes_it() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let snapshot = fixture.snapshot(&host);
    let item = snapshot["needs"][0]["id"]
        .as_str()
        .expect("need")
        .to_owned();
    let revision = snapshot["preferences"]["revision"]
        .as_str()
        .expect("revision")
        .to_owned();
    let snoozed = host
        .set_preferences(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "expected_revision": revision,
            "snooze": {"item_id": item, "until": "2026-01-01T01:00:00.000Z"},
        }))
        .expect("snooze");
    assert_eq!(
        snoozed["preferences"]["snoozed"][&item],
        "2026-01-01T01:00:00.000Z"
    );
    let restored = host
        .set_preferences(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "expected_revision": snoozed["preferences"]["revision"],
            "snooze": {"item_id": item, "until": "2026-01-01T00:00:00.000Z"},
        }))
        .expect("restore");
    assert!(
        restored["preferences"]["snoozed"].get(&item).is_none(),
        "a time already passed ends the snooze and is dropped"
    );
    let too_long = host
        .set_preferences(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "expected_revision": restored["preferences"]["revision"],
            "snooze": {"item_id": item, "until": "2026-01-09T00:00:00.000Z"},
        }))
        .expect_err("over seven days");
    assert_eq!(too_long.code, "E_VALIDATION");
}

#[test]
fn preferences_and_vault_configuration_are_local_and_revisioned() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let snapshot = fixture.snapshot(&host);
    let preferences = snapshot["preferences"]["revision"]
        .as_str()
        .expect("revision")
        .to_owned();
    let updated = host
        .set_preferences(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "expected_revision": preferences,
            "theme": "dark",
            "motion": false,
        }))
        .expect("preferences");
    assert_eq!(updated["preferences"]["theme"], "dark");
    assert_eq!(updated["preferences"]["motion"], false);
    let stale = host
        .set_preferences(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "expected_revision": preferences,
            "theme": "light",
        }))
        .expect_err("stale preferences revision");
    assert_eq!(stale.code, "E_STALE_ANSWER");

    let persisted = Preferences::load(&fixture.data).expect("load preferences");
    assert_eq!(persisted.theme, "dark");
    let config = Config::load(&fixture.data)
        .expect("config")
        .expect("config exists");
    assert!(config.vault.is_some());
}

#[test]
fn guard_classification_matches_design_6_3() {
    let easy = card("easy", "high", false, "Wake.");
    assert_eq!(guard::guard_for(&easy), guard::Guard::Easy);
    let low = card("easy", "low", false, "Wake.");
    assert_eq!(guard::guard_for(&low), guard::Guard::Choose);
    let costly = card("costly", "high", false, "Wake.");
    assert_eq!(guard::guard_for(&costly), guard::Guard::Choose);
    let irreversible = card("irreversible", "high", false, "Wake.");
    assert_eq!(guard::guard_for(&irreversible), guard::Guard::Confirm);
    let external = card("easy", "high", true, "Wake.");
    assert_eq!(guard::guard_for(&external), guard::Guard::Confirm);
    let heuristic = card("easy", "high", false, "Deploy the migration tonight.");
    assert_eq!(guard::guard_for(&heuristic), guard::Guard::Confirm);
}

#[test]
fn link_repo_records_a_local_mapping_without_a_vault_event() {
    let scratch = scratch(Vec::new());
    let clock = ManualClock::new();
    let (host, _ui, _sink) = scratch.host(clock);
    let other = scratch._root.path().join("other-repo");
    std::fs::create_dir_all(&other).expect("other repo");
    let event_lines_before = host.event_lines().len();
    let data = host
        .link_repo(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "repo": other.to_string_lossy(),
            "project": "demo",
        }))
        .expect("link repo");
    assert_eq!(data["project"], "demo");
    assert_eq!(
        host.event_lines().len(),
        event_lines_before,
        "no Vault event"
    );
    let config = Config::load(&scratch.data)
        .expect("config")
        .expect("present");
    let canonical = other.canonicalize().expect("canonical");
    assert_eq!(
        config.repos.get(&canonical.to_string_lossy().into_owned()),
        Some(&ProjectSlug::parse("demo").expect("slug"))
    );
    assert!(!scratch.vault.join("config.json").exists());
}

#[test]
fn unlink_repo_removes_only_the_local_mapping() {
    let scratch = scratch(Vec::new());
    let clock = ManualClock::new();
    let (host, _ui, _sink) = scratch.host(clock);
    let other = scratch._root.path().join("other-repo");
    std::fs::create_dir_all(&other).expect("other repo");
    let linked = host
        .link_repo(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "repo": other.to_string_lossy(),
            "project": "demo",
        }))
        .expect("link repo");
    let key = linked["repo"].as_str().expect("canonical key").to_owned();
    let event_lines_before = host.event_lines().len();
    let data = host
        .unlink_repo(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "repo": key,
            "expected_project": "demo",
        }))
        .expect("unlink repo");
    assert_eq!(data["removed"], true);
    assert_eq!(data["project"], "demo");
    assert_eq!(
        data["project_still_linked"], true,
        "the scratch repo still links demo"
    );
    assert_eq!(
        host.event_lines().len(),
        event_lines_before,
        "no Vault event"
    );
    let config = Config::load(&scratch.data)
        .expect("config")
        .expect("present");
    assert!(!config.repos.contains_key(&key), "the link is gone");
    assert!(other.is_dir(), "the repository itself is untouched");
    let again = host
        .unlink_repo(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "repo": key,
        }))
        .expect("unlink again");
    assert_eq!(again["removed"], false);
}

#[test]
fn unlink_repo_works_after_agents_rewrite_the_config_behind_settings() {
    let scratch = scratch(Vec::new());
    let clock = ManualClock::new();
    let (host, _ui, _sink) = scratch.host(clock);
    let _ = host.get_snapshot();
    let demo = ProjectSlug::parse("demo").expect("slug");
    let first = scratch._root.path().join("cli-repo");
    std::fs::create_dir_all(&first).expect("cli repo");
    // `agendar project link` writes the same local config behind the app's back.
    Config::map_repo(&scratch.data, &first, &demo).expect("cli link");
    let local = host.get_local_config().expect("local config");
    let key = first
        .canonicalize()
        .expect("canonical")
        .to_string_lossy()
        .into_owned();
    assert!(
        local["repos"].get(&key).is_some(),
        "Settings lists the CLI link"
    );
    // While Settings is open, another agent starts in another project: it
    // selects that project, which rewrites the config.
    Config::select(
        &scratch.data,
        None,
        Some(&ProjectSlug::parse("other").expect("slug")),
    )
    .expect("agent start");
    let data = host
        .unlink_repo(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "repo": key,
            "expected_project": "demo",
        }))
        .expect("unlink is not refused by an unrelated config change");
    assert_eq!(data["removed"], true);
    assert_eq!(
        data["project_still_linked"], true,
        "the scratch repo still links demo, so Settings keeps the project"
    );
    let last = Config::load(&scratch.data)
        .expect("config")
        .expect("present")
        .repos
        .into_iter()
        .find(|(_, project)| project == &demo)
        .map(|(path, _)| path)
        .expect("the scratch repo link");
    let data = host
        .unlink_repo(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "repo": last,
            "expected_project": "demo",
        }))
        .expect("unlink the last folder");
    assert_eq!(
        data["project_still_linked"], false,
        "no folder links demo now"
    );
}

#[test]
fn unlink_repo_refuses_when_the_folder_now_links_another_project() {
    let scratch = scratch(Vec::new());
    let clock = ManualClock::new();
    let (host, _ui, _sink) = scratch.host(clock);
    let other = scratch._root.path().join("moved-repo");
    std::fs::create_dir_all(&other).expect("repo");
    Config::map_repo(
        &scratch.data,
        &other,
        &ProjectSlug::parse("other").expect("slug"),
    )
    .expect("link to other");
    let key = other
        .canonicalize()
        .expect("canonical")
        .to_string_lossy()
        .into_owned();
    let error = host
        .unlink_repo(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "repo": key,
            "expected_project": "demo",
        }))
        .expect_err("Settings showed demo, the folder now links other");
    assert_eq!(error.code, "E_STALE_ANSWER");
    let config = Config::load(&scratch.data)
        .expect("config")
        .expect("present");
    assert!(config.repos.contains_key(&key), "the link is kept");
}

#[cfg(unix)]
#[test]
fn resolve_rescue_hands_a_lost_feature_to_a_new_session() {
    use agendar_core::model::{DemoKind, PlanFeature, ProjectDeclared};

    let root = tempfile::tempdir().expect("root");
    let vault = root.path().join("vault");
    let data = root.path().join("data");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    let (service, _) = Service::init(&vault, &data).expect("init");
    let project = ProjectSlug::parse("demo").expect("project");
    service.select_config(Some(&project)).expect("select");
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn child");
    let dead_pid = child.id();
    let dead_start = agendar_core::runtime::sessions::process_identity(dead_pid).expect("identity");
    let session = register_session(
        &service,
        SessionRegistration {
            project: project.clone(),
            harness: Harness::Generic,
            harness_session_id: "first".to_owned(),
            pid: dead_pid,
            process_start: dead_start,
            repo_root: repo.clone(),
        },
    )
    .expect("register first session");
    service
        .declare_project(
            &project,
            &session,
            ProjectDeclared {
                agent: None,
                title: Some("Handoff".to_owned()),
                brief: Some("A lost session leaves one feature.".to_owned()),
                goal: Some("Hand it to the next session.".to_owned()),
                glyph: Some("radar".to_owned()),
                archived: None,
                icon: None,
            },
            None,
        )
        .expect("declare project");
    let node = NodePath::parse("core/lost").expect("node");
    service
        .declare_feature(
            &project,
            &session,
            node.clone(),
            PlanFeature {
                agent: None,
                title: Some("Lost work".to_owned()),
                depends_on: None,
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
    service
        .progress_started(
            &project,
            &session,
            &node,
            agendar_core::model::EtaRange {
                min_s: agendar_core::model::Seconds::new(600).expect("min"),
                max_s: agendar_core::model::Seconds::new(1_200).expect("max"),
            },
            None,
            None,
        )
        .expect("start work");
    child.kill().expect("kill child");
    child.wait().expect("reap child");
    agendar_core::runtime::steward::steward_tick(&service).expect("loss tick");

    let ui = Arc::new(FakeUi::default());
    let sink = FakeSink::available();
    let host = Host::new(&data, ManualClock::new(), ui, sink).expect("host");
    let snapshot = host.get_snapshot().expect("snapshot");
    let need = snapshot["needs"]
        .as_array()
        .expect("needs")
        .iter()
        .find(|need| {
            need["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("rescue:lost:"))
        })
        .expect("lost rescue")
        .clone();
    // The user may first dismiss the item (Cancel): it closes on this computer
    // only, and the step can still be handed off from its project page.
    let decide = snapshot["needs"]
        .as_array()
        .expect("needs")
        .iter()
        .find(|need| need["kind"] != "rescue")
        .and_then(|need| need["id"].as_str())
        .map(str::to_owned);
    if let Some(decide) = decide {
        let refused = host
            .dismiss_need(&json!({
                "request_id": ulid::Ulid::generate().to_string(),
                "item_id": decide,
            }))
            .expect_err("only hand-off items can be dismissed");
        assert_eq!(refused.code, "E_NOT_FOUND");
    }
    let dismissed = host
        .dismiss_need(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "item_id": need["id"],
        }))
        .expect("dismiss the hand-off item");
    assert_eq!(dismissed["preferences"]["dismissed"], json!([need["id"]]));
    let revision = need["revision"].as_str().expect("revision").to_owned();
    let request_id = ulid::Ulid::generate().to_string();
    let first = host
        .resolve_rescue(&json!({
            "request_id": request_id,
            "item_id": need["id"],
            "expected_rescue_revision": revision,
            "action": "handoff",
        }))
        .expect("handoff");
    let retry = host
        .resolve_rescue(&json!({
            "request_id": request_id,
            "item_id": need["id"],
            "expected_rescue_revision": revision,
            "action": "handoff",
        }))
        .expect("idempotent retry");
    assert_eq!(first["event_id"], retry["event_id"]);

    // Without a new session the handoff is only intent; the replacement then
    // consumes it as an explicit cause.
    let replacement = register_session(
        &service,
        SessionRegistration {
            project: project.clone(),
            harness: Harness::Generic,
            harness_session_id: "replacement".to_owned(),
            pid: std::process::id(),
            process_start: agendar_core::runtime::sessions::process_identity(std::process::id())
                .expect("identity"),
            repo_root: repo,
        },
    )
    .expect("register replacement");
    let started = service
        .progress_started(
            &project,
            &replacement,
            &node,
            agendar_core::model::EtaRange {
                min_s: agendar_core::model::Seconds::new(60).expect("min"),
                max_s: agendar_core::model::Seconds::new(120).expect("max"),
            },
            None,
            None,
        )
        .expect("replacement start");
    assert!(started["attempt"].is_string());
}

// ---------------------------------------------------------------------------
// Delivery review, Seen checks, reports and media
// ---------------------------------------------------------------------------

fn publish(fixture: &Scratch, ext: BlobExt, bytes: &[u8]) -> BlobRef {
    let service = fixture.service();
    let mut reader = bytes;
    put_blob(
        service.vault(),
        service.local(),
        &fixture.project,
        service.host(),
        ext,
        &mut reader,
    )
    .expect("publish blob")
}

struct DeliveryBoard {
    service: Service,
    node: NodePath,
    attempt: EventId,
    session: agendar_core::model::SessionId,
    receipt: EventId,
}

fn delivery_board(fixture: &Scratch) -> DeliveryBoard {
    let service = fixture.service();
    let fresh = service
        .refresh(
            UtcTime::parse("2026-01-01T00:10:00.000Z").expect("time"),
            false,
        )
        .expect("refresh");
    let project_state = &fresh.state.projects[&fixture.project];
    let (ask, node_name) = fixture.asks[0].clone();
    let node = NodePath::parse(&node_name).expect("node");
    let attempt = project_state.features[&node]
        .attempt
        .clone()
        .expect("attempt");
    let session = project_state
        .sessions
        .keys()
        .next()
        .cloned()
        .expect("session");
    let receipt = service
        .answer_decision(
            &fixture.project,
            &ask,
            &agendar_core::model::OptionId::parse("adapters").expect("option"),
            None,
            None,
            None,
        )
        .expect("answer");
    DeliveryBoard {
        service,
        node,
        attempt,
        session,
        receipt,
    }
}

fn deliver_body(name: &str, evidence: Option<BlobRef>, demo: Option<BlobRef>) -> Delivery {
    let criterion = || agendar_core::model::CriterionId::parse("continues").expect("criterion");
    Delivery {
        agent: Agent {
            session: agendar_core::model::SessionId::parse("00000000000000000000000101")
                .expect("session"),
            harness: Harness::Generic,
        },
        name: name.to_owned(),
        demos: vec![Demo::CliTranscript(CliTranscriptDemo {
            blob: demo.unwrap_or_else(|| BlobRef {
                host: agendar_core::model::HostId::parse("h-00000000000000000000000001")
                    .expect("host"),
                sha256: agendar_core::model::Sha256::from_bytes(b"fixture"),
                ext: BlobExt::Txt,
                bytes: 7,
            }),
            check: None,
        })],
        criteria: vec![CriterionResult {
            criterion: criterion(),
            status: agendar_core::model::CheckResult::Passed,
            claim: Claim::AgentSays,
            evidence,
            observation: None,
        }],
        disclosure: Disclosure {
            proven: vec!["Agent ran the demo".to_owned()],
            not_proven_yet: vec!["Independent check".to_owned()],
            faked_for_now: Vec::new(),
        },
    }
}

fn delivery_value<'a>(project: &'a Value, id: &EventId) -> &'a Value {
    project["project"]["deliveries"]
        .as_object()
        .expect("deliveries")
        .get(id.as_str())
        .expect("delivery value")
}

#[test]
fn review_delivery_guards_the_rendered_revision_and_saves_a_rule_once() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let board = delivery_board(&fixture);
    let body = deliver_body("Wake delivery", None, None);
    let delivered = board
        .service
        .deliver(
            &fixture.project,
            &board.session,
            &board.node,
            &board.attempt,
            &board.receipt,
            None,
            body,
            None,
        )
        .expect("deliver");
    let delivery_id =
        EventId::parse(delivered["event_id"].as_str().expect("delivery")).expect("id");
    drop(board.service);

    let (host, ui, _sink) = fixture.host(ManualClock::new());
    let project = host
        .get_project(&json!({"project": "demo"}))
        .expect("project");
    let delivery = delivery_value(&project, &delivery_id);
    let revision = delivery["revision"].as_str().expect("revision").to_owned();
    let snapshot = host.get_snapshot().expect("snapshot");
    assert!(
        snapshot["needs"]
            .as_array()
            .expect("needs")
            .iter()
            .any(|need| need["kind"] == "review"
                && need["source"].as_str() == Some(delivery_id.as_str())),
        "the pending delivery is routed to the review queue"
    );
    let before = host.event_lines().len();

    let stale = host
        .review_delivery(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "delivery_id": delivery_id,
            "expected_delivery_revision": "0".repeat(64),
            "verdict": "accept",
        }))
        .expect_err("stale revision");
    assert_eq!(stale.code, "E_STALE_ANSWER");
    assert_eq!(
        host.event_lines().len(),
        before,
        "no review event was written"
    );

    let accepted = host
        .review_delivery(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "delivery_id": delivery_id,
            "expected_delivery_revision": revision,
            "verdict": "accept",
        }))
        .expect("accept");
    assert!(accepted["event_id"].is_string());
    let project = host
        .get_project(&json!({"project": "demo"}))
        .expect("project");
    assert_eq!(
        delivery_value(&project, &delivery_id)["review"].as_str(),
        Some("accepted")
    );
    let snapshot = host.get_snapshot().expect("snapshot");
    assert!(
        snapshot["needs"]
            .as_array()
            .expect("needs")
            .iter()
            .all(|need| need["source"].as_str() != Some(delivery_id.as_str())),
        "an accepted delivery leaves the review queue"
    );

    let revision = delivery_value(&project, &delivery_id)["revision"]
        .as_str()
        .expect("revision")
        .to_owned();
    let sent_back = host
        .review_delivery(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "delivery_id": delivery_id,
            "expected_delivery_revision": revision,
            "verdict": "send_back",
            "reason": "Check disagrees",
            "save_rule": {"rule_id": "check-bytes", "text": "Compare bytes before delivery"},
        }))
        .expect("send back");
    assert!(sent_back["rule_event"].is_string());
    let project = host
        .get_project(&json!({"project": "demo"}))
        .expect("project");
    let project_value = &project["project"];
    assert_eq!(
        delivery_value(&project, &delivery_id)["review"].as_str(),
        Some("sent_back")
    );
    let rules = project_value["rules"].as_array().expect("rules");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["value"]["rule_id"], "check-bytes");
    assert!(!ui.changed.lock().expect("changed").is_empty());
}

#[test]
fn reports_are_source_text_and_media_is_referenced_and_expiring() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let html = publish(&fixture, BlobExt::Html, b"<script>alert(1)</script>");
    let image = publish(&fixture, BlobExt::Png, b"\x89PNG\r\n\x1a\nfake image bytes");
    let unreferenced = publish(&fixture, BlobExt::Txt, b"not attached to anything\n");
    let board = delivery_board(&fixture);
    let body = Delivery {
        demos: vec![
            Demo::CliTranscript(CliTranscriptDemo {
                blob: publish(&fixture, BlobExt::Txt, b"transcript\n"),
                check: None,
            }),
            Demo::Screenshot(ScreenshotDemo {
                blob: image.clone(),
                alt: "Wake screenshot".to_owned(),
                check: None,
            }),
        ],
        criteria: vec![CriterionResult {
            criterion: agendar_core::model::CriterionId::parse("continues").expect("criterion"),
            status: agendar_core::model::CheckResult::Passed,
            claim: Claim::AgentSays,
            evidence: Some(html.clone()),
            observation: None,
        }],
        ..deliver_body("Reports", Some(html.clone()), None)
    };
    let delivered = board
        .service
        .deliver(
            &fixture.project,
            &board.session,
            &board.node,
            &board.attempt,
            &board.receipt,
            None,
            body,
            None,
        )
        .expect("deliver");
    assert!(delivered["event_id"].is_string());
    drop(board.service);

    let clock = ManualClock::new();
    let (host, _ui, _sink) = fixture.host(Arc::clone(&clock));
    let report = host
        .read_report(&json!({"project": "demo", "blob": html}))
        .expect("report");
    assert_eq!(report["mime"], "text/html");
    assert_eq!(report["text"], "<script>alert(1)</script>");
    assert!(
        report.get("media_token").is_none(),
        "HTML stays source text, never an active media token"
    );

    let media = host
        .read_report(&json!({"project": "demo", "blob": image}))
        .expect("media report");
    assert_eq!(media["mime"], "image/png");
    let token = media["media_token"].as_str().expect("token").to_owned();
    let (mime, bytes) = host
        .media_response(&format!("/{token}"))
        .expect("verified media");
    assert_eq!(mime, "image/png");
    assert!(bytes.starts_with(b"\x89PNG"));
    assert!(host.media_response(&format!("/../{token}")).is_err());
    assert!(
        host.media_response("/00000000000000000000000000000000")
            .is_err(),
        "unknown tokens are refused"
    );
    assert!(
        host.read_report(&json!({"project": "demo", "blob": unreferenced}))
            .is_err(),
        "an unreferenced blob is never readable"
    );
    clock.advance(61);
    assert!(
        host.media_response(&format!("/{token}")).is_err(),
        "media tokens expire after sixty seconds"
    );
}

#[test]
fn option_images_on_an_open_question_are_served_as_media() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let image = publish(
        &fixture,
        BlobExt::Png,
        b"\x89PNG\r\n\x1a\noption image bytes",
    );
    let clock = ManualClock::new();
    {
        let (host, _ui, _sink) = fixture.host(Arc::clone(&clock));
        assert!(
            host.read_report(&json!({"project": "demo", "blob": image}))
                .is_err(),
            "an image no card references is not readable"
        );
    }
    let service = fixture.service();
    let fresh = service
        .refresh(
            UtcTime::parse("2026-01-01T00:10:00.000Z").expect("time"),
            false,
        )
        .expect("refresh");
    let session = fresh.state.projects[&fixture.project]
        .sessions
        .keys()
        .next()
        .cloned()
        .expect("session");
    let mut pictured = card("easy", "med", false, "Pick the look.");
    pictured.options[1].image = Some(image.clone());
    service
        .ask(
            &fixture.project,
            &session,
            &NodePath::parse("core/wake").expect("node"),
            pictured,
            agendar_core::model::Mode::Ask,
            None,
        )
        .expect("ask with an option image");
    drop(service);

    let (host, _ui, _sink) = fixture.host(Arc::clone(&clock));
    let media = host
        .read_report(&json!({"project": "demo", "blob": image}))
        .expect("option image");
    assert_eq!(media["mime"], "image/png");
    let token = media["media_token"].as_str().expect("token");
    let (mime, bytes) = host
        .media_response(&format!("/{token}"))
        .expect("verified media");
    assert_eq!(mime, "image/png");
    assert!(bytes.ends_with(b"option image bytes"));
}

#[test]
fn an_approved_check_publishes_a_seen_observation() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let board = delivery_board(&fixture);
    let repo = fixture._root.path().join("repo");
    let mut body = deliver_body("Checkable", None, None);
    if let Demo::CliTranscript(demo) = &mut body.demos[0] {
        demo.check = Some(DemoCheck {
            criterion: agendar_core::model::CriterionId::parse("continues").expect("criterion"),
            argv: vec!["agendar-check-probe".to_owned(), "file_digest".to_owned()],
            cwd: repo.to_string_lossy().into_owned(),
            timeout_s: 30,
        });
    }
    let delivered = board
        .service
        .deliver(
            &fixture.project,
            &board.session,
            &board.node,
            &board.attempt,
            &board.receipt,
            None,
            body,
            None,
        )
        .expect("deliver");
    let delivery_id =
        EventId::parse(delivered["event_id"].as_str().expect("delivery")).expect("id");
    drop(board.service);
    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let project = host
        .get_project(&json!({"project": "demo"}))
        .expect("project");
    let delivery = delivery_value(&project, &delivery_id);
    assert_eq!(
        delivery["demos"][0]["body"]["check"]["criterion"], "continues",
        "the demo body is projected with its proposed check for the Delivery view"
    );
    let revision = delivery["revision"].as_str().expect("revision").to_owned();
    let exe = std::env::current_exe()
        .expect("test exe")
        .to_string_lossy()
        .into_owned();
    let profile = json!({
        "id": "continues-check",
        "criterion": "continues",
        "argv": [exe.clone(), "--list"],
        "cwd": repo.to_string_lossy(),
        "timeout_s": 30,
    });
    let normalized = CheckProfile {
        id: None,
        argv: vec![exe, "--list".to_owned()],
        args: Vec::new(),
        executable: None,
        cwd: Some(repo.to_string_lossy().into_owned()),
        repo_root: None,
        timeout_s: Some(30),
        delivery_id: None,
        criterion: Some(agendar_core::model::CriterionId::parse("continues").expect("criterion")),
    }
    .normalize()
    .expect("normalize");
    let approved = host
        .approve_check(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "expected_approval_revision": check_runtime::absent_revision(&normalized),
            "profile": profile.clone(),
            "consent": true,
        }))
        .expect("approve");
    let approval_id = approved["approval_id"]
        .as_str()
        .expect("approval id")
        .to_owned();

    let unapproved = host
        .run_check(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "project": "demo",
            "delivery_id": delivery_id,
            "criterion": "continues",
            "approval_id": "0".repeat(64),
            "expected_delivery_revision": revision,
            "expected_approval_revision": "0".repeat(64),
        }))
        .expect_err("unapproved check");
    assert_eq!(unapproved.code, "E_NOT_FOUND");

    let ran = host
        .run_check(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "project": "demo",
            "delivery_id": delivery_id,
            "criterion": "continues",
            "approval_id": approval_id,
            "expected_delivery_revision": revision,
            "expected_approval_revision": approval_id,
        }))
        .expect("run check");
    assert_eq!(ran["result"], "passed");
    let project = host
        .get_project(&json!({"project": "demo"}))
        .expect("project");
    let criterion = &delivery_value(&project, &delivery_id)["criteria"][0];
    assert_eq!(criterion["seen_result"], "passed");
    assert_eq!(criterion["provenance"], "seen");
    assert_eq!(criterion["agent_result"], "passed");
}

#[test]
fn set_rule_uses_the_expected_revision_and_keeps_one_identity() {
    let fixture = scratch(Vec::new());
    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let project = ProjectSlug::parse("demo").expect("project");
    let absent = agendar_core::model::rule_revision(
        &project,
        &agendar_core::model::RuleId::parse("no-shell").expect("rule"),
        None,
    );
    let created = host
        .set_rule(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "project": "demo",
            "expected_revision": absent,
            "rule": {"rule_id": "no-shell", "text": "Use explicit argv.", "enabled": true},
        }))
        .expect("create rule");
    assert!(created["event_id"].is_string());

    let after = host
        .get_project(&json!({"project": "demo"}))
        .expect("project");
    let rules = after["project"]["rules"].as_array().expect("rules");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["value"]["rule_id"], "no-shell");
    let current = rules[0]["revision"].as_str().expect("revision").to_owned();

    host.set_rule(&json!({
        "request_id": ulid::Ulid::generate().to_string(),
        "project": "demo",
        "expected_revision": current,
        "rule": {"rule_id": "no-shell", "text": "Use explicit argv, never a shell.", "enabled": true},
    }))
    .expect("update rule");

    let stale = host
        .set_rule(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "project": "demo",
            "expected_revision": absent,
            "rule": {"rule_id": "no-shell", "text": "Stale write", "enabled": true},
        }))
        .expect_err("stale rule revision");
    assert_eq!(stale.code, "E_STALE_ANSWER");
}

#[test]
fn a_cli_proposed_check_is_projected_and_runs_only_after_approval() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let board = delivery_board(&fixture);
    let repo = fixture._root.path().join("repo");
    let demo = repo.join("demo.txt");
    std::fs::write(&demo, b"demo transcript\n").expect("demo");
    let evidence = repo.join("evidence.txt");
    std::fs::write(&evidence, b"aggregate: passed\n").expect("evidence");
    let exe = std::env::current_exe()
        .expect("test exe")
        .to_string_lossy()
        .into_owned();
    let cwd = repo.to_string_lossy().into_owned();
    // Exactly the `agendar deliver --json` input shape, parsed by the CLI itself:
    // the proposed check rides on the demo body it describes (`DemoCheck`).
    let input: agendar_cli::delivery::DeliveryInput = serde_json::from_value(json!({
        "name": "Checkable delivery",
        "demos": [{
            "kind": "cli_transcript",
            "body": {
                "file": demo.to_string_lossy(),
                "ext": "txt",
                "check": {
                    "criterion": "continues",
                    "argv": [exe.clone(), "--list"],
                    "cwd": cwd.clone(),
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
            "proven": ["Agent ran the demo"],
            "not_proven_yet": ["Independent check"],
            "faked_for_now": []
        },
        // The second declared carrier: a proposal kept in this host's journal.
        "checks": [{
            "criterion": "continues",
            "argv": [exe.clone(), "--help"],
            "cwd": cwd.clone(),
            "timeout_s": 15
        }]
    }))
    .expect("delivery input");
    let (body, _uploaded) = agendar_cli::delivery::build_body(
        &board.service,
        &fixture.project,
        &board.session,
        Harness::Generic,
        &input,
    )
    .expect("delivery body");
    let delivered = board
        .service
        .deliver(
            &fixture.project,
            &board.session,
            &board.node,
            &board.attempt,
            &board.receipt,
            None,
            body,
            None,
        )
        .expect("deliver");
    let delivery_id =
        EventId::parse(delivered["event_id"].as_str().expect("delivery")).expect("id");
    agendar_cli::delivery::validate_proposals(
        &board.service,
        &fixture.project,
        Some(&delivery_id),
        &input,
    )
    .expect("store the journal proposal");
    drop(board.service);

    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let project = host
        .get_project(&json!({"project": "demo"}))
        .expect("project");
    let delivery = delivery_value(&project, &delivery_id);
    // The real snapshot DTO carries the exact profile the view displays, on the
    // same demo body member the CLI wrote: no second, frontend-only shape.
    let proposal = &delivery["demos"][0]["body"]["check"];
    assert_eq!(proposal["criterion"], "continues");
    assert_eq!(proposal["argv"], json!([exe.clone(), "--list"]));
    assert_eq!(proposal["timeout_s"], 30);
    // The CLI stores the canonical repository path, so what is displayed is what
    // the approval rule hashes.
    let projected_cwd = proposal["cwd"].as_str().expect("projected cwd").to_owned();
    assert_eq!(
        projected_cwd,
        repo.canonicalize()
            .expect("canonical repo")
            .to_string_lossy()
    );
    // The local journal carrier is projected next to the event carrier.
    let journal = delivery["check_proposals"]
        .as_array()
        .expect("projected proposals");
    assert_eq!(journal.len(), 1, "one journal proposal");
    assert_eq!(journal[0]["criterion"], "continues");
    assert_eq!(journal[0]["argv"], json!([exe.clone(), "--help"]));
    assert_eq!(journal[0]["timeout_s"], 15);
    assert_eq!(
        journal[0]["digest"].as_str().map(str::len),
        Some(64),
        "the projection carries the profile digest"
    );

    let normalized = CheckProfile {
        id: None,
        argv: vec![exe.clone(), "--list".to_owned()],
        args: Vec::new(),
        executable: None,
        cwd: Some(projected_cwd.clone()),
        repo_root: None,
        timeout_s: Some(30),
        delivery_id: None,
        criterion: None,
    }
    .normalize()
    .expect("normalize");
    let profile = json!({
        "argv": [exe.clone(), "--list"],
        "cwd": projected_cwd,
        "timeout_s": 30
    });

    // A token that is neither this profile's absence nor its digest is stale,
    // not an approval: the field is a real SHA-256 precondition.
    let stale = host
        .approve_check(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "expected_approval_revision": "1".repeat(64),
            "profile": profile.clone(),
            "consent": true,
        }))
        .expect_err("stale approval revision");
    assert_eq!(stale.code, "E_STALE_ANSWER");

    // The first real approval sends the absence revision for this exact profile.
    let approved = host
        .approve_check(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "expected_approval_revision": check_runtime::absent_revision(&normalized),
            "profile": profile.clone(),
            "consent": true,
        }))
        .expect("approve");
    let approval_id = approved["approval_id"]
        .as_str()
        .expect("approval id")
        .to_owned();
    assert_eq!(
        approval_id.as_str(),
        check_runtime::profile_digest(&normalized).as_str(),
        "the approval id is the profile digest"
    );

    let revision = delivery["revision"].as_str().expect("revision").to_owned();
    let ran = host
        .run_check(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "project": "demo",
            "delivery_id": delivery_id,
            "criterion": "continues",
            "approval_id": approval_id,
            "expected_delivery_revision": revision,
            "expected_approval_revision": approval_id,
        }))
        .expect("run check");
    assert_eq!(ran["result"], "passed");
    let project = host
        .get_project(&json!({"project": "demo"}))
        .expect("project");
    let criterion = &delivery_value(&project, &delivery_id)["criteria"][0];
    assert_eq!(criterion["provenance"], "seen");
    assert_eq!(criterion["seen_result"], "passed");
}

#[test]
fn later_slice_commands_report_a_typed_unsupported_error() {
    for command in [
        "update_project",
        "update_feature",
        "set_policy",
        "open_preview",
    ] {
        let error = agendar_app::commands::unsupported_capability(command);
        assert_eq!(error.code, "E_UNSUPPORTED", "{command}");
        assert!(error.message.contains(command));
        assert!(!error.retryable);
    }
}

#[test]
fn agenda_and_policy_commands_guard_their_revisions() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        false,
    )]);
    let (host, _ui, _sink) = fixture.host(ManualClock::new());

    let project = host
        .get_project(&json!({"project": "demo"}))
        .expect("project");
    let goal_token = project["project"]["field_revisions"]["goal"]
        .as_str()
        .expect("goal token")
        .to_owned();
    let request_id = ulid::Ulid::generate().to_string();
    let created = host
        .update_project(&json!({
            "request_id": request_id,
            "project": "demo",
            "expected_fields": {"goal": goal_token},
            "patch": {"goal": "Keep the plan human-owned"},
        }))
        .expect("update the goal");
    assert!(created["event_id"].is_string());

    // An identical request id replays the journaled result; a different payload
    // under the same id is a conflict.
    let retry = host
        .update_project(&json!({
            "request_id": request_id,
            "project": "demo",
            "expected_fields": {"goal": goal_token},
            "patch": {"goal": "Keep the plan human-owned"},
        }))
        .expect("replay the journaled result");
    assert_eq!(retry, created);
    let conflict = host
        .update_project(&json!({
            "request_id": request_id,
            "project": "demo",
            "expected_fields": {"goal": goal_token},
            "patch": {"goal": "A different goal"},
        }))
        .expect_err("same request id, different payload");
    assert_eq!(conflict.code, "E_REQUEST_CONFLICT");

    let stale = host
        .update_project(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "project": "demo",
            "expected_fields": {"goal": goal_token},
            "patch": {"goal": "Stale goal"},
        }))
        .expect_err("the first token is stale now");
    assert_eq!(stale.code, "E_STALE_ANSWER");
    assert!(!stale.hint.is_empty());

    let project = host
        .get_project(&json!({"project": "demo"}))
        .expect("project");
    assert_eq!(
        project["project"]["summary"]["goal"],
        "Keep the plan human-owned"
    );
    let feature = &project["project"]["features"]["core/wake"];
    let title = feature["field_revisions"]["title"]
        .as_str()
        .expect("title token")
        .to_owned();
    let priority = feature["field_revisions"]["priority"]
        .as_str()
        .expect("priority token")
        .to_owned();
    host.update_feature(&json!({
        "request_id": ulid::Ulid::generate().to_string(),
        "project": "demo",
        "node": "core/wake",
        "expected_fields": {"title": title, "priority": priority},
        "patch": {"title": "Human feature", "priority": 0},
    }))
    .expect("update the feature");
    let missing = host
        .update_feature(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "project": "demo",
            "node": "core/wake",
            "expected_fields": {"title": title},
            "patch": {"title": "No priority token", "priority": 1},
        }))
        .expect_err("expected_fields must match the patched fields");
    assert_eq!(missing.code, "E_VALIDATION");

    let project = host
        .get_project(&json!({"project": "demo"}))
        .expect("project");
    assert_eq!(
        project["project"]["features"]["core/wake"]["title"],
        "Human feature"
    );
    assert_eq!(project["project"]["features"]["core/wake"]["priority"], 0);

    // Policy revisions come from the projection; the backend rechecks them and
    // keeps irreversible cells locked.
    let policy = project["project"]["policy"].clone();
    let revision = policy["revision"].as_str().expect("revision").to_owned();
    let mut strict = policy["value"].clone();
    strict["preset"] = json!("strict");
    strict["cells"] = json!({
        "easy_local": "ask", "easy_wide": "ask",
        "costly_local": "ask", "costly_wide": "ask",
        "irreversible_local": "ask", "irreversible_wide": "ask",
    });
    strict["allow_timeouts"] = json!(false);
    host.set_policy(&json!({
        "request_id": ulid::Ulid::generate().to_string(),
        "project": "demo",
        "expected_revision": revision,
        "policy": strict,
    }))
    .expect("strict policy");
    let stale = host
        .set_policy(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "project": "demo",
            "expected_revision": revision,
            "policy": strict,
        }))
        .expect_err("stale policy revision");
    assert_eq!(stale.code, "E_STALE_ANSWER");

    let mut loosened = strict.clone();
    loosened["preset"] = json!("custom");
    loosened["cells"]["irreversible_local"] = json!("auto");
    loosened["allow_timeouts"] = json!(true);
    let project = host
        .get_project(&json!({"project": "demo"}))
        .expect("project");
    let loosened_error = host
        .set_policy(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "project": "demo",
            "expected_revision": project["project"]["policy"]["revision"],
            "policy": loosened,
        }))
        .expect_err("irreversible cells stay locked");
    assert_eq!(loosened_error.code, "E_VALIDATION");

    let snapshot = host.get_snapshot().expect("snapshot");
    assert_eq!(
        snapshot["state"]["projects"]["demo"]["policy"]["value"]["preset"],
        "strict"
    );
    for section in [
        "delivered",
        "coming_up",
        "needs_you",
        "if_you_wait",
        "by_default",
        "at_risk",
    ] {
        assert!(
            snapshot["brief"][section].is_array(),
            "brief section {section}"
        );
    }
}

// ---------------------------------------------------------------------------
// Live pulses, change batches and the unblock transition
// ---------------------------------------------------------------------------

/// Append one foreign-host pulse through the real writer path helpers.
fn append_foreign_pulse(
    scratch: &Scratch,
    host: &str,
    session: &str,
    node: &NodePath,
    attempt: &EventId,
    ts: &str,
    id: &str,
) {
    use std::io::Write as _;
    let vault = agendar_core::vault::Vault::open(&scratch.vault).expect("vault");
    let writer = agendar_core::model::WriterId::new(
        agendar_core::model::HostId::parse(host).expect("host id"),
        agendar_core::model::WriterRole::Agent(
            agendar_core::model::SessionId::parse(session).expect("session"),
        ),
    );
    let directory =
        agendar_core::vault::ensure_host_directory(vault.root(), &scratch.project, writer.host())
            .expect("host directory");
    let name = agendar_core::vault::writer_file_name(&writer, 1).expect("writer file");
    let value = json!({
        "v": 1,
        "id": id,
        "seq": 1,
        "writer": writer.to_string(),
        "ts": ts,
        "project": scratch.project.as_str(),
        "refs": {"node": node.as_str(), "attempt": attempt.as_str()},
        "kind": "pulse",
        "body": {
            "agent": {"session": session, "harness": "generic"},
            "message": "Still shaping the wake path.",
            "boundary": false,
        },
    });
    let mut line = serde_json::to_string(&value).expect("pulse serializes");
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(name))
        .expect("open writer file");
    file.write_all(line.as_bytes()).expect("append pulse");
    file.flush().expect("flush");
}

#[test]
fn a_change_batch_reports_each_new_pulse_once_with_its_own_origin() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let clock = ManualClock::new();
    clock.advance(5 * 60);
    let (host, ui, _sink) = fixture.host(Arc::clone(&clock));
    let snapshot = fixture.snapshot(&host);
    let project = &snapshot["state"]["projects"]["demo"];
    let node = NodePath::parse("core/wake").expect("node");
    let attempt = EventId::parse(
        project["features"]["core/wake"]["attempt"]
            .as_str()
            .expect("attempt"),
    )
    .expect("attempt id");

    // Hydration never animates history.
    assert_eq!(ui.changed.lock().expect("changed").len(), 0);
    // Later reads must not consume publication state: a snapshot after this one
    // still has to let the next tick report what it did not publish.
    fixture.snapshot(&host);
    host.watch_tick("time");
    let quiet = ui.changed.lock().expect("changed").last().cloned();
    let quiet = quiet.expect("one quiet tick");
    assert_eq!(quiet["batch"], "time");
    assert_eq!(quiet["pulses"], json!([]));

    // A pulse from another host arrives as one `sync` batch with its own time.
    append_foreign_pulse(
        &fixture,
        "h-00000000000000000000000002",
        "00000000000000000000000201",
        &node,
        &attempt,
        "2026-01-01T00:07:30.000Z",
        "00000000000000000000000701",
    );
    clock.advance(1);
    // A read between the write and the tick must not swallow the pulse.
    fixture.snapshot(&host);
    host.watch_tick("sync");
    let synced = ui.changed.lock().expect("changed").last().cloned();
    let synced = synced.expect("one sync batch");
    assert_eq!(synced["batch"], "sync");
    let pulses = synced["pulses"].as_array().expect("pulses");
    assert_eq!(pulses.len(), 1);
    assert_eq!(pulses[0]["id"], "00000000000000000000000701");
    assert_eq!(pulses[0]["project"], "demo");
    assert_eq!(pulses[0]["node"], "core/wake");
    assert_eq!(pulses[0]["host"], "h-00000000000000000000000002");
    assert_eq!(pulses[0]["event_ts"], "2026-01-01T00:07:30.000Z");

    // The same pulse is never reported twice.
    clock.advance(1);
    host.watch_tick("sync");
    let again = ui.changed.lock().expect("changed").last().cloned();
    let again = again.expect("second sync batch");
    assert_eq!(again["pulses"], json!([]));
    assert_eq!(again["batch"], "sync", "an empty scan keeps the scan kind");

    // This host's own agent pulse is a `local` batch.
    let service = fixture.service();
    let session = agendar_core::model::SessionId::parse(
        project["features"]["core/wake"]["session"]
            .as_str()
            .expect("session"),
    )
    .expect("session id");
    service
        .pulse(
            &fixture.project,
            &session,
            &node,
            &attempt,
            "Continuing here.".to_owned(),
            false,
            None,
            None,
        )
        .expect("local pulse");
    clock.advance(1);
    host.watch_tick("sync");
    let local = ui.changed.lock().expect("changed").last().cloned();
    let local = local.expect("local batch");
    assert_eq!(local["batch"], "local");
    let pulses = local["pulses"].as_array().expect("pulses");
    assert_eq!(pulses.len(), 1);
    assert_eq!(
        pulses[0]["host"],
        <agendar_core::model::HostId as AsRef<str>>::as_ref(service.host())
    );
    assert_ne!(pulses[0]["event_ts"], "2026-01-01T00:07:30.000Z");
}

#[test]
fn an_unblock_is_emitted_once_for_the_answer_and_once_for_the_ack() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let clock = ManualClock::new();
    let (host, ui, _sink) = fixture.host(clock);
    let snapshot = fixture.snapshot(&host);
    let (ask, _) = fixture.asks[0].clone();
    let rev = revision(&snapshot, "demo", &ask);
    let prepared = fixture.prepare(&host, &ask, "adapters", &rev);
    let token = prepared["token"].as_str().expect("token").to_owned();
    let label = prepared["label"].as_str().expect("label").to_owned();
    let answered = fixture.answer(&host, &token, &label).expect("answer");
    let answer_id = answered["event_id"].as_str().expect("event id").to_owned();

    let unblocks = ui.unblock.lock().expect("unblock").clone();
    assert_eq!(unblocks.len(), 1, "one answered unblock");
    assert_eq!(unblocks[0]["phase"], "answered");
    assert_eq!(unblocks[0]["answer_id"], answer_id);
    assert_eq!(unblocks[0]["project"], "demo");
    assert_eq!(unblocks[0]["from"], format!("decision:{ask}"));
    assert_eq!(
        unblocks[0]["released"],
        json!(["core/wake"]),
        "the answered feature is released when its own gate opens"
    );

    // A background tick never repeats the answered phase.
    host.watch_tick("sync");
    assert_eq!(ui.unblock.lock().expect("unblock").len(), 1);

    // The agent acks through the CLI: exactly one `acked` phase follows.
    let service = fixture.service();
    let session = agendar_core::model::SessionId::parse(
        fixture.snapshot(&host)["state"]["projects"]["demo"]["features"]["core/wake"]["session"]
            .as_str()
            .expect("session"),
    )
    .expect("session id");
    service
        .ack(
            &fixture.project,
            &session,
            &EventId::parse(&answer_id).expect("answer id"),
            Vec::new(),
            None,
        )
        .expect("ack");
    host.watch_tick("sync");
    let unblocks = ui.unblock.lock().expect("unblock").clone();
    assert_eq!(unblocks.len(), 2, "answered then acked");
    assert_eq!(unblocks[1]["phase"], "acked");
    assert_eq!(unblocks[1]["answer_id"], answer_id);
    assert_eq!(unblocks[1]["project"], "demo");
    assert_eq!(
        unblocks[1]["released"],
        json!(["core/wake"]),
        "the ack repeats the set the answer released"
    );

    // A second valid ack of the same answer (another agent, or a re-send) is a
    // fact about that answer, not a new phase.
    service
        .ack(
            &fixture.project,
            &session,
            &EventId::parse(&answer_id).expect("answer id"),
            Vec::new(),
            None,
        )
        .expect("second ack");
    host.watch_tick("sync");
    assert_eq!(
        ui.unblock.lock().expect("unblock").len(),
        2,
        "one acked phase per answered decision"
    );

    // A further tick adds nothing.
    host.watch_tick("sync");
    host.watch_tick("time");
    assert_eq!(ui.unblock.lock().expect("unblock").len(), 2);
}

#[test]
fn an_answer_publication_reports_a_foreign_pulse_that_arrived_before_it() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let clock = ManualClock::new();
    clock.advance(5 * 60);
    let (host, ui, _sink) = fixture.host(Arc::clone(&clock));
    let snapshot = fixture.snapshot(&host);
    let project = &snapshot["state"]["projects"]["demo"];
    let node = NodePath::parse("core/wake").expect("node");
    let attempt = EventId::parse(
        project["features"]["core/wake"]["attempt"]
            .as_str()
            .expect("attempt"),
    )
    .expect("attempt id");
    let (ask, _) = fixture.asks[0].clone();
    let rev = revision(&snapshot, "demo", &ask);
    let prepared = fixture.prepare(&host, &ask, "adapters", &rev);
    let token = prepared["token"].as_str().expect("token").to_owned();
    let label = prepared["label"].as_str().expect("label").to_owned();

    // Another host's pulse lands between prepare and answer.
    append_foreign_pulse(
        &fixture,
        "h-00000000000000000000000002",
        "00000000000000000000000201",
        &node,
        &attempt,
        "2026-01-01T00:07:30.000Z",
        "00000000000000000000000702",
    );
    clock.advance(1);
    fixture.answer(&host, &token, &label).expect("answer");

    // The answer's own publication carries it, tagged as synced.
    let published = ui.changed.lock().expect("changed").last().cloned();
    let published = published.expect("answer publication");
    assert_eq!(published["batch"], "sync");
    let pulses = published["pulses"].as_array().expect("pulses");
    assert_eq!(pulses.len(), 1);
    assert_eq!(pulses[0]["id"], "00000000000000000000000702");
    assert_eq!(pulses[0]["event_ts"], "2026-01-01T00:07:30.000Z");

    // And the next tick does not report it again.
    clock.advance(1);
    host.watch_tick("sync");
    let again = ui.changed.lock().expect("changed").last().cloned();
    assert_eq!(again.expect("tick")["pulses"], json!([]));
}

#[test]
fn the_tray_attention_count_follows_needs_you() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let clock = ManualClock::new();
    let (host, ui, _sink) = fixture.host(clock);
    let snapshot = fixture.snapshot(&host);
    // Hydration reports the open ask.
    assert_eq!(ui.attention.lock().expect("attention").last(), Some(&1));

    let (ask, _) = fixture.asks[0].clone();
    let rev = revision(&snapshot, "demo", &ask);
    let prepared = fixture.prepare(&host, &ask, "adapters", &rev);
    let token = prepared["token"].as_str().expect("token").to_owned();
    let label = prepared["label"].as_str().expect("label").to_owned();
    fixture.answer(&host, &token, &label).expect("answer");
    // Answering clears it, so the tray returns to its idle glyph.
    assert_eq!(ui.attention.lock().expect("attention").last(), Some(&0));
}

#[test]
fn the_project_graph_is_the_reducer_dto() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let clock = ManualClock::new();
    let (host, _ui, _sink) = fixture.host(clock);
    let project = host
        .get_project(&json!({"project": "demo"}))
        .expect("project");
    let graph = &project["graph"];
    let (ask, _) = fixture.asks[0].clone();
    let mut ids: Vec<&str> = graph["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .map(|node| node["id"].as_str().expect("node id"))
        .collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        vec![format!("decision:{ask}").as_str(), "feature:core/wake",]
    );
    let decision = graph["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|node| node["kind"] == "decision")
        .expect("decision node");
    assert_eq!(decision["need_id"], format!("decide:{ask}"));
    assert_eq!(decision["feature"], "core/wake");
    assert_eq!(decision["state"], "open");
    let feature = graph["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|node| node["kind"] == "feature")
        .expect("feature node");
    assert_eq!(feature["state"], "doing");
    assert_eq!(feature["assumed"], false);
    assert!(feature.get("need_id").is_none(), "features own no need id");
    let edges = graph["edges"].as_array().expect("edges");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0]["from"], format!("decision:{ask}"));
    assert_eq!(edges[0]["to"], "feature:core/wake");
    assert_eq!(edges[0]["source_event"], format!("{ask}"));
}

// ---------------------------------------------------------------------------
// Activity feed
// ---------------------------------------------------------------------------

fn activity_texts(project: &Value) -> Vec<String> {
    project["activity"]
        .as_array()
        .expect("every project carries an activity array")
        .iter()
        .map(|item| item["text"].as_str().expect("text").to_owned())
        .collect()
}

#[test]
fn every_snapshot_project_carries_its_activity_feed() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let snapshot = fixture.snapshot(&host);
    let project = &snapshot["state"]["projects"]["demo"];
    let items = project["activity"].as_array().expect("activity");
    assert!(!items.is_empty() && items.len() <= 40);
    // Header statistics ride along, non-canonical like the feed.
    let stats = project["stats"].as_object().expect("stats");
    for key in [
        "agents_running",
        "agents_idle",
        "agents_stalled",
        "finished_24h",
        "agent_ms_24h",
        "timed_steps",
        "instant_excluded",
    ] {
        assert!(stats[key].is_u64(), "{key} is a count");
    }
    assert!(stats.get("tokens").is_none(), "tokens are not reported yet");
    for item in items {
        let mut keys: Vec<&str> = item
            .as_object()
            .expect("item object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "actor",
                "actor_kind",
                "count",
                "detail",
                "icon",
                "msg",
                "node",
                "text",
                "ts"
            ]
        );
        assert!(item["count"].as_u64().is_some_and(|count| count >= 1));
    }
    let ask = items
        .iter()
        .find(|item| item["icon"] == "help-circle")
        .expect("the ask is in the feed");
    assert_eq!(
        ask["text"],
        "Agent asked you: How should answers reach idle agents?"
    );
    assert_eq!(ask["actor"], "Agent");
    assert_eq!(ask["actor_kind"], "agent");
    assert_eq!(ask["node"], "core/wake");
    assert!(
        activity_texts(project).contains(&"Agent described the project".to_owned()),
        "{:?}",
        activity_texts(project)
    );
    // Activity is a local UI extra: the canonical export never carries it.
    let exported = serde_json::to_value(
        fixture
            .service()
            .export_state(UtcTime::parse("2026-01-01T00:00:00.000Z").expect("time"))
            .expect("export"),
    )
    .expect("export json");
    assert!(exported["projects"]["demo"].get("activity").is_none());

    // A human answer shows up as the human's own line, also on `get_project`.
    let (ask_id, _) = fixture.asks[0].clone();
    let rev = revision(&snapshot, "demo", &ask_id);
    let prepared = fixture.prepare(&host, &ask_id, "adapters", &rev);
    let token = prepared["token"].as_str().expect("token").to_owned();
    fixture
        .answer(&host, &token, "Use harness adapters")
        .expect("answer");
    let detail = host
        .get_project(&json!({"project": "demo"}))
        .expect("project");
    let answer = detail["project"]["activity"]
        .as_array()
        .expect("project activity")
        .iter()
        .find(|item| item["icon"] == "user")
        .expect("the answer is in the feed")
        .clone();
    assert_eq!(
        answer["text"],
        "You answered \u{201c}How should answers reach idle agents?\u{201d} \u{2192} Use harness adapters"
    );
    assert_eq!(answer["actor"], "You");
    assert_eq!(answer["actor_kind"], "human");
    assert_eq!(answer["count"], 1);
}

#[test]
fn a_finished_milestone_notifies_once_and_a_restart_does_not_replay_it() {
    use agendar_core::model::{DemoKind, PlanFeature, WorkStatus};
    let fixture = scratch(vec![]);
    let clock = ManualClock::new();
    let (host, _ui, sink) = fixture.host(clock.clone());
    let snapshot = fixture.snapshot(&host);
    fixture.enable_notifications(&host, &snapshot);
    host.watch_tick("time");
    assert!(sink.shown().is_empty(), "the first tick only seeds");

    let service = fixture.service();
    let (pid, process_start) =
        agendar_core::runtime::sessions::process_identity(std::process::id())
            .map(|start| (std::process::id(), start))
            .expect("test process identity");
    let session = register_session(
        &service,
        SessionRegistration {
            project: fixture.project.clone(),
            harness: Harness::Generic,
            harness_session_id: "progress-test".to_owned(),
            pid,
            process_start,
            repo_root: fixture.data.clone(),
        },
    )
    .expect("register session");
    let node = NodePath::parse("m/one").expect("node");
    service
        .declare_feature(
            &fixture.project,
            &session,
            node.clone(),
            PlanFeature {
                agent: None,
                title: Some("Only step".to_owned()),
                depends_on: None,
                slice: Some("m".to_owned()),
                demo_kind: Some(DemoKind::CliTranscript),
                milestone: Some("M1".to_owned()),
                deadline: None,
                priority: None,
                paused: None,
                handoff: None,
            },
            None,
        )
        .expect("declare feature");
    let started = service
        .progress_started(
            &fixture.project,
            &session,
            &node,
            agendar_core::model::EtaRange {
                min_s: agendar_core::model::Seconds::new(60).expect("seconds"),
                max_s: agendar_core::model::Seconds::new(120).expect("seconds"),
            },
            None,
            None,
        )
        .expect("start");
    let attempt = EventId::parse(started["attempt"].as_str().expect("attempt")).expect("id");
    host.watch_tick("sync");
    assert!(sink.shown().is_empty(), "starting is not news");

    service
        .progress_finished(
            &fixture.project,
            &session,
            &node,
            &attempt,
            WorkStatus::Done,
            Vec::new(),
            None,
            None,
        )
        .expect("done");
    host.watch_tick("sync");
    let shown = sink.shown();
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert_eq!(shown[0].title, "Project Demo: M1 finished");
    assert_eq!(shown[0].body, "1 of 1 steps done · nothing left");
    assert_eq!(shown[0].item_id, "project:demo");
    assert!(shown[0].choices.is_empty(), "Open only");
    host.watch_tick("time");
    assert_eq!(sink.shown().len(), 1, "once");

    // A fresh host (an app restart) seeds silently.
    let (restarted, _ui, again) = fixture.host(clock);
    let snapshot = fixture.snapshot(&restarted);
    fixture.enable_notifications(&restarted, &snapshot);
    restarted.watch_tick("time");
    restarted.watch_tick("time");
    assert!(again.shown().is_empty());
}

#[test]
fn a_connected_agent_with_nothing_in_progress_is_idle_not_working() {
    // The session is live (this test process) but never starts its step.
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        false,
    )]);
    let (host, ui, _sink) = fixture.host(ManualClock::new());
    let snapshot = fixture.snapshot(&host);
    let stats = &snapshot["state"]["projects"]["demo"]["stats"];
    assert_eq!(stats["agents_running"], 0);
    assert_eq!(stats["agents_idle"], 1);
    assert_eq!(
        ui.working.lock().expect("working").last(),
        Some(&0),
        "the menu bar counts no working project"
    );
}

#[test]
fn an_open_question_shows_whether_an_agent_is_waiting_for_it() {
    use std::sync::atomic::AtomicBool;
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let (ask, _) = fixture.asks[0].clone();
    let listening = |host: &Host| {
        fixture.snapshot(host)["state"]["projects"]["demo"]["listening"][ask.as_str()].clone()
    };
    assert_eq!(
        listening(&host),
        "connected",
        "connected, nobody waiting yet"
    );

    let snapshot = fixture.snapshot(&host);
    let session = snapshot["state"]["projects"]["demo"]["sessions"]
        .as_object()
        .and_then(|sessions| sessions.keys().next().cloned())
        .expect("a session");
    let session = agendar_core::model::SessionId::parse(&session).expect("session id");
    let cancel = Arc::new(AtomicBool::new(false));
    let waiter = {
        let service = fixture.service();
        let project = fixture.project.clone();
        let ask = ask.clone();
        let cancel = Arc::clone(&cancel);
        std::thread::spawn(move || {
            let options = agendar_core::service::WaitOptions {
                timeout: Some(std::time::Duration::from_secs(20)),
                cancel: Some(cancel),
                ..agendar_core::service::WaitOptions::default()
            };
            let mut observer = agendar_core::service::NoopWaitObserver;
            let _ = service.wait_for_answer(&project, &session, &ask, &options, &mut observer);
        })
    };
    let mut seen = Value::Null;
    for _ in 0..50 {
        seen = listening(&host);
        if seen == "waiting" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(seen, "waiting", "a blocked wait holds a fresh lease");
    cancel.store(true, Ordering::SeqCst);
    waiter.join().expect("waiter");
    assert_eq!(
        listening(&host),
        "connected",
        "the lease ends with the wait"
    );
}

#[test]
fn an_answered_question_takes_its_notice_down_but_a_late_click_still_routes() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let clock = ManualClock::new();
    let (host, _ui, sink) = fixture.host(Arc::clone(&clock));
    let snapshot = fixture.snapshot(&host);
    fixture.enable_notifications(&host, &snapshot);
    fixture.snapshot(&host);
    let notice = sink.shown().into_iter().next().expect("notice");

    // Answered in the app: the next tick withdraws the visible notice once.
    let (ask, _) = fixture.asks[0].clone();
    let snapshot = fixture.snapshot(&host);
    let rev = revision(&snapshot, "demo", &ask);
    let prepared = fixture.prepare(&host, &ask, "adapters", &rev);
    let token = prepared["token"].as_str().expect("token").to_owned();
    let label = prepared["label"].as_str().expect("label").to_owned();
    fixture.answer(&host, &token, &label).expect("answer");
    host.watch_tick("sync");
    host.watch_tick("sync");
    assert_eq!(
        sink.withdrawn(),
        vec![notice.id.clone()],
        "withdrawn exactly once"
    );
    // A click on the old banner still routes instead of being unknown.
    let outcome = host.handle_notice(&notice.id, NoticeAction::Open, Origin::Live);
    assert_ne!(outcome, NoticeOutcome::Unknown);
}

#[test]
fn a_test_notice_shows_now_and_a_reported_refusal_reaches_the_snapshot() {
    let fixture = scratch(vec![]);
    let (host, _ui, sink) = fixture.host(ManualClock::new());
    let result = host.test_notice().expect("test notice");
    assert_eq!(result["sent"], true);
    let shown = sink.shown();
    assert_eq!(shown.len(), 1);
    assert!(shown[0].body.starts_with("Test notification"));
    assert!(shown[0].choices.is_empty());
    assert!(
        sink.refreshed.load(Ordering::SeqCst) >= 1,
        "permission re-read"
    );

    *sink.last_error.lock().expect("last error") =
        Some("Notifications are not allowed for this application".to_owned());
    let snapshot = fixture.snapshot(&host);
    assert_eq!(
        snapshot["local"]["notification_error"],
        "Notifications are not allowed for this application"
    );
}

#[test]
fn each_option_of_an_easy_question_is_its_own_notification_button() {
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "low", false, "Wake is blocked."),
        true,
    )]);
    let (host, _ui, sink) = fixture.host(ManualClock::new());
    let snapshot = fixture.snapshot(&host);
    fixture.enable_notifications(&host, &snapshot);
    fixture.snapshot(&host);
    let notice = sink.shown().into_iter().next().expect("notice");
    assert_eq!(
        notice.choices,
        vec![
            "Use harness adapters".to_owned(),
            "Use inbox on next start".to_owned()
        ],
        "every option, recommended first, even at low confidence: pressing one is explicit"
    );
    let outcome = host.handle_notice(&notice.id, NoticeAction::Choose(1), Origin::Live);
    assert_eq!(outcome, NoticeOutcome::Answered { duplicate: false });
    let (ask, _) = fixture.asks[0].clone();
    let snapshot = fixture.snapshot(&host);
    let receipt = fixture.decision(&snapshot, &ask)["effective_receipt"]
        .as_str()
        .expect("receipt")
        .to_owned();
    assert_eq!(
        snapshot["state"]["projects"]["demo"]["receipts"][receipt.as_str()]["chose"]["id"],
        "inbox"
    );
    // The other button is now a duplicate, not a second answer.
    let again = host.handle_notice(&notice.id, NoticeAction::Choose(0), Origin::Live);
    assert_eq!(again, NoticeOutcome::Answered { duplicate: true });
    assert_eq!(fixture.answer_count(&host), 1);
}

#[test]
fn agents_seen_without_the_skill_appear_once_and_skill_sessions_are_left_out() {
    use agendar_core::runtime::presence::{Observation, PresenceState, record};
    let fixture = scratch(vec![]);
    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let repo = fixture.data.parent().expect("root").join("repo");
    std::fs::create_dir_all(repo.join("src")).expect("src");
    let now = UtcTime::parse("2026-01-01T00:00:00.000Z").expect("time");
    // The scratch skill session runs in this test process.
    record(
        &fixture.data,
        "claude_code:skill",
        "claude_code",
        &repo,
        Some(std::process::id()),
        Observation::State(PresenceState::Working),
        now,
    )
    .expect("record skill agent");
    record(
        &fixture.data,
        "claude_code:plain",
        "claude_code",
        &repo.join("src"),
        None,
        Observation::State(PresenceState::Waiting),
        now,
    )
    .expect("record plain agent");
    let snapshot = fixture.snapshot(&host);
    let rows = snapshot["presence"].as_array().expect("presence");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["key"], "claude_code:plain");
    assert_eq!(rows[0]["state"], "waiting");
    assert_eq!(rows[0]["project"], "demo", "matched by its linked folder");
    assert_eq!(rows[0]["folder"], "src");
}

#[test]
fn open_report_refuses_a_blob_the_project_never_referenced() {
    let fixture = scratch(vec![]);
    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let error = host
        .open_report(&json!({
            "project": "demo",
            "blob": {
                "host": "h-00000000000000000000000001",
                "sha256": "e".repeat(64),
                "bytes": 10,
                "ext": "html",
            },
        }))
        .expect_err("an unreferenced blob never opens");
    assert_eq!(error.code.as_str(), "E_NOT_FOUND");
}

#[test]
fn an_svg_project_icon_is_served_as_media_with_its_exact_type() {
    use agendar_core::model::ProjectDeclared;
    let fixture = scratch(vec![(
        "core/wake",
        card("easy", "med", false, "Wake is blocked."),
        true,
    )]);
    let svg_bytes: &[u8] =
        br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 8"><rect width="8" height="8" fill="#f80"/></svg>"##;
    let icon = publish(&fixture, BlobExt::Svg, svg_bytes);
    let stray = publish(
        &fixture,
        BlobExt::Svg,
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
    );
    let board = delivery_board(&fixture);
    board
        .service
        .declare_project(
            &fixture.project,
            &board.session,
            ProjectDeclared {
                agent: None,
                title: None,
                brief: None,
                goal: None,
                glyph: None,
                archived: None,
                icon: Some(icon.clone()),
            },
            None,
        )
        .expect("declare icon");
    drop(board.service);

    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let media = host
        .read_report(&json!({"project": "demo", "blob": icon}))
        .expect("icon media");
    assert_eq!(media["mime"], "image/svg+xml");
    let token = media["media_token"].as_str().expect("token");
    let (mime, bytes) = host.media_response(&format!("/{token}")).expect("bytes");
    assert_eq!(mime, "image/svg+xml");
    assert_eq!(bytes, svg_bytes);
    assert!(
        host.read_report(&json!({"project": "demo", "blob": stray}))
            .is_err(),
        "an svg that is not the project icon is never served"
    );
}

#[test]
fn an_answer_in_your_own_words_reaches_the_receipt() {
    let fixture = scratch(vec![(
        "core/wake",
        card("costly", "med", false, "Wake is blocked."),
        false,
    )]);
    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let snapshot = fixture.snapshot(&host);
    let (ask, _) = fixture.asks[0].clone();
    let rev = revision(&snapshot, "demo", &ask);
    let own = |surface: &str, text: &str| {
        host.prepare_answer(
            surface,
            &json!({
                "ask_id": ask.as_str(),
                "option_id": "own",
                "own_answer": text,
                "expected_decision_revision": rev,
            }),
        )
    };
    assert!(
        own("popover", "Poll every minute").is_err(),
        "own words only from the main window"
    );
    assert_eq!(own("main", "   ").expect_err("empty").code, "E_VALIDATION");
    let text = "Neither: poll the inbox every minute,\nand log each wake.";
    let prepared = own("main", text).expect("prepare own answer");
    assert_eq!(prepared["receipt"]["chose"]["label"], "Your own answer");
    let token = prepared["token"].as_str().expect("token").to_owned();
    fixture
        .answer(&host, &token, "Your own answer")
        .expect("own answer commits");
    let after = fixture.snapshot(&host);
    let decision = &after["state"]["projects"]["demo"]["decisions"][ask.as_str()];
    let receipt_id = decision["effective_receipt"].as_str().expect("receipt");
    let receipt = &after["state"]["projects"]["demo"]["receipts"][receipt_id];
    assert_eq!(receipt["chose"]["id"], "own");
    assert_eq!(receipt["own_answer"], text);
    assert_eq!(
        receipt["ruled_out"].as_array().map(Vec::len),
        Some(2),
        "every card option is ruled out"
    );
}

#[test]
fn the_language_preference_is_saved_and_validated() {
    let fixture = scratch(vec![]);
    let (host, _ui, _sink) = fixture.host(ManualClock::new());
    let snapshot = fixture.snapshot(&host);
    assert_eq!(
        snapshot["preferences"]["language"], "auto",
        "follows the system by default"
    );
    let revision = snapshot["preferences"]["revision"]
        .as_str()
        .expect("revision")
        .to_owned();
    let saved = host
        .set_preferences(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "expected_revision": revision,
            "language": "zh-Hant",
        }))
        .expect("save language");
    assert_eq!(saved["preferences"]["language"], "zh-Hant");
    let revision = saved["preferences"]["revision"]
        .as_str()
        .expect("revision")
        .to_owned();
    let refused = host
        .set_preferences(&json!({
            "request_id": ulid::Ulid::generate().to_string(),
            "expected_revision": revision,
            "language": "fr",
        }))
        .expect_err("unknown language");
    assert_eq!(refused.code, "E_VALIDATION");
}
