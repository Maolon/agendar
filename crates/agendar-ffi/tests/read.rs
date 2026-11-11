#![forbid(unsafe_code)]

//! Contract v1 reads (`refresh`, `now`, `projects`, `project`, `card`) on an
//! empty Vault, on the generated workload, and on hand-built states. A second
//! host reads the same Vault through the core, and the rows must agree with
//! it: order, counts and revisions.

// The synthetic workload the iOS fixture Vault is generated from.
#[path = "../../agendar-core/tests/support/mod.rs"]
mod support;
#[path = "../../agendar-core/tests/workload/mod.rs"]
mod workload;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use agendar_core::guard::{decision_revision, find_decision, need_revision};
use agendar_core::index::query::recent_project_events;
use agendar_core::model::{EventId, NeedsYouItem, NodePath, ProjectSlug, UtcTime};
use agendar_core::reduce::attention::is_active_need;
use agendar_core::reduce::{activity, brief, ordering, stats};
use agendar_core::service::{Fresh, Service};
use agendar_core::vault::{Clock, SystemClock, Vault};
use agendar_ffi::{
    ActivityKind, AskMode, CardStatus, Confidence, FactClaim, FfiError, Guard, NeedKind,
    RefusalCode, Risk, Session, StepGate, StepProgress,
};
use serde_json::{Value, json};

const MAC: &str = "h-00000000000000000000000001";
const SESSION: &str = "00000000000000000000000101";
const PROJECT: &str = "mobile";

/// One Vault made by the Mac, read by a mobile client (the facade) and by a
/// third host through the core (what the desktop reads).
struct World {
    dir: tempfile::TempDir,
    next_id: u64,
    seqs: BTreeMap<String, u64>,
    now: UtcTime,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        Service::init(&dir.path().join("vault"), &dir.path().join("mac")).expect("init");
        Self {
            dir,
            next_id: 0,
            seqs: BTreeMap::new(),
            now: SystemClock.now(),
        }
    }

    fn vault(&self) -> PathBuf {
        self.dir.path().join("vault")
    }

    /// The facade as one more host with its own data directory.
    fn open(&self, name: &str) -> Session {
        Session::open(
            self.vault().display().to_string(),
            self.dir.path().join(name).display().to_string(),
        )
        .expect("open")
    }

    /// The same Vault as the core reduces it for the desktop.
    fn core(&self) -> Fresh {
        Service::open(&self.vault(), &self.dir.path().join("mac"))
            .expect("core")
            .refresh(SystemClock.now(), false)
            .expect("core refresh")
    }

    /// Append one event line `age_s` seconds old, as its writer's host would.
    fn push_in(
        &mut self,
        project: &str,
        age_s: i64,
        writer: &str,
        kind: &str,
        refs: Value,
        body: Value,
    ) -> String {
        self.next_id += 1;
        let id = format!("{:026}", self.next_id);
        let seq = self.seqs.entry(format!("{project}/{writer}")).or_insert(0);
        *seq += 1;
        let line = json!({
            "v": 1, "id": id, "seq": *seq, "writer": writer,
            "ts": self.now.add_seconds(-age_s).canonical(),
            "project": project, "refs": refs, "kind": kind, "body": body,
        });
        let (host, file) = writer.split_once('/').expect("writer");
        let host_dir = self.vault().join("projects").join(project).join(host);
        std::fs::create_dir_all(&host_dir).expect("host dir");
        let file = host_dir.join(format!("{file}.jsonl"));
        let mut text = std::fs::read_to_string(&file).unwrap_or_default();
        text.push_str(&serde_json::to_string(&line).expect("json"));
        text.push('\n');
        std::fs::write(&file, text).expect("write line");
        id
    }

    fn push(&mut self, age_s: i64, writer: &str, kind: &str, refs: Value, body: Value) -> String {
        self.push_in(PROJECT, age_s, writer, kind, refs, body)
    }

    fn declare(&mut self, age_s: i64) {
        self.push(
            age_s,
            &operator(),
            "project.declared",
            json!({}),
            json!({"brief": "Fixture", "goal": "Answer from a mobile client", "title": "Mobile"}),
        );
    }

    fn step(&mut self, age_s: i64, node: &str, title: &str, depends_on: &[&str]) {
        self.push(
            age_s,
            &operator(),
            "plan.feature",
            json!({"node": node}),
            json!({"demo_kind": "cli_transcript", "depends_on": depends_on, "slice": "s0", "title": title}),
        );
    }

    fn start(&mut self, age_s: i64, node: &str) -> String {
        self.push(
            age_s,
            &agent_writer(),
            "work.progress",
            json!({"node": node}),
            json!({"agent": agent(), "eta": {"max_s": 900, "min_s": 300}, "evidence": [], "status": "started"}),
        )
    }

    fn ask(&mut self, age_s: i64, node: &str, card: Value, supersedes: Option<&str>) -> String {
        let mut refs = json!({"node": node});
        if let Some(previous) = supersedes {
            refs["supersedes"] = json!(previous);
        }
        self.push(
            age_s,
            &agent_writer(),
            "decision.ask",
            refs,
            json!({"agent": agent(), "policy": policy(&card), "card": card}),
        )
    }

    /// A declared project with one started step and one open ask on it.
    fn with_open_ask(card: Value) -> (Self, String) {
        let mut world = Self::new();
        world.declare(600);
        world.step(590, "n0", "First step", &[]);
        world.start(580, "n0");
        let ask = world.ask(570, "n0", card, None);
        (world, ask)
    }
}

fn operator() -> String {
    format!("{MAC}/operator")
}

fn agent_writer() -> String {
    format!("{MAC}/s-{SESSION}")
}

fn agent() -> Value {
    json!({"harness": "generic", "session": SESSION})
}

/// The policy evaluation an ask declares: the card's worst-case risk.
fn policy(card: &Value) -> Value {
    let options = card["options"].as_array().expect("options");
    let rank = |value: &Value| match value["reversibility"].as_str() {
        Some("irreversible") => 2,
        Some("costly") => 1,
        _ => 0,
    };
    let worst = options.iter().map(rank).max().unwrap_or(0).max(rank(card));
    let external = card["external"] == json!(true)
        || options
            .iter()
            .any(|option| option["external"] == json!(true));
    let reversibility = ["easy", "costly", "irreversible"][worst];
    json!({
        "budget": {"blocked": false, "demo_gap_s": 0, "pending_slices": 0, "reasons": [], "speculative_features": 0},
        "effective": "ask", "external": external, "heuristic_hits": [], "requested": "ask",
        "reversibility": reversibility
    })
}

/// Two easy options with a medium-confidence recommendation: guard `easy`.
fn card() -> Value {
    json!({
        "already_checked": [{"text": "Hooks are installed.", "claim": "agent_says"}],
        "domain": "arch",
        "done_when": [{"id": "bytes", "text": "Demo bytes match."}],
        "external": false, "on_timeout": {"kind": "hold"},
        "options": [
            {"consequence": "Use hooks.", "eta": {"max_s": 3600, "min_s": 1800}, "external": false,
             "icon": "link", "id": "adapters", "label": "Use harness adapters", "reversibility": "easy"},
            {"consequence": "Use the inbox.", "cost": "One more start", "external": false,
             "icon": "inbox", "id": "inbox", "label": "Use inbox on next start", "reversibility": "easy"}
        ],
        "question": "How should answers reach idle agents?", "reach": "local",
        "recommended": {"because": "It is simpler.", "confidence": "med", "option": "adapters"},
        "reversibility": "easy", "why_now": "Wake delivery is blocked."
    })
}

fn core_code(error: FfiError) -> String {
    match error {
        FfiError::Core { code, .. } => code,
        other => panic!("{other}"),
    }
}

/// The core's active Needs-you list across projects, in the core's order.
fn core_needs(fresh: &Fresh) -> Vec<NeedsYouItem> {
    let mut needs: Vec<NeedsYouItem> = fresh
        .state
        .projects
        .values()
        .filter(|project| !project.summary.archived)
        .flat_map(|project| project.needs_you.iter())
        .filter(|need| is_active_need(need))
        .cloned()
        .collect();
    ordering::sort_needs(&mut needs);
    needs
}

fn event(id: &str) -> EventId {
    EventId::parse(id).expect("event id")
}

#[test]
fn every_read_works_on_an_empty_vault() {
    let world = World::new();
    let mobile = world.open("mobile");

    // Before the first refresh the cached index is empty, not an error.
    assert!(mobile.projects().expect("projects").is_empty());
    assert!(mobile.now(None).expect("now").needs.is_empty());

    let freshness = mobile.refresh(2_000).expect("refresh");
    assert_eq!(freshness.events, 0);
    assert!(freshness.hosts.is_empty() && freshness.waiting.is_empty());
    assert!(freshness.this_host.starts_with("h-"), "{freshness:?}");
    UtcTime::parse(&freshness.evaluated_at).expect("evaluated_at is a core time");

    let board = mobile
        .now(Some("2026-01-15T09:00:00Z".to_owned()))
        .expect("now");
    assert!(board.needs.is_empty() && board.snoozed.is_empty());
    assert!(board.moving.is_empty() && board.stalled.is_empty());
    assert!(board.done_since.is_empty());
    assert!(mobile.projects().expect("projects").is_empty());

    let unknown = "00000000000000000000000004".to_owned();
    assert_eq!(
        core_code(mobile.project("nope".to_owned()).expect_err("project")),
        "E_NOT_FOUND"
    );
    assert_eq!(
        core_code(mobile.card(unknown).expect_err("card")),
        "E_NOT_FOUND"
    );
    assert_eq!(
        core_code(mobile.card("not an id".to_owned()).expect_err("card id")),
        "E_VALIDATION"
    );
    assert_eq!(
        core_code(mobile.project("Not A Slug".to_owned()).expect_err("slug")),
        "E_VALIDATION"
    );
    assert_eq!(
        core_code(mobile.now(Some("yesterday".to_owned())).expect_err("time")),
        "E_VALIDATION"
    );
}

#[test]
fn only_refresh_reads_the_vault() {
    let (mut world, ask) = World::with_open_ask(card());
    let mobile = world.open("mobile");
    assert!(mobile.now(None).expect("now").needs.is_empty());
    assert_eq!(
        core_code(mobile.card(ask.clone()).expect_err("unread ask")),
        "E_NOT_FOUND"
    );

    let first = mobile.refresh(2_000).expect("refresh");
    assert_eq!(first.events, 4);
    assert_eq!(mobile.now(None).expect("now").needs.len(), 1);

    // New lines in the Vault are not seen until the next refresh.
    world.step(10, "n1", "Second step", &["n0"]);
    assert_eq!(
        mobile
            .project(PROJECT.to_owned())
            .expect("page")
            .steps
            .len(),
        1
    );
    let second = mobile.refresh(2_000).expect("refresh");
    assert_eq!(second.events, 5);
    assert!(second.generation > first.generation);
    assert_ne!(second.event_set_digest, first.event_set_digest);
    assert_eq!(
        mobile
            .project(PROJECT.to_owned())
            .expect("page")
            .steps
            .len(),
        2
    );
}

#[test]
fn an_open_ask_is_a_row_and_a_card_with_the_desktop_revision() {
    let (world, ask) = World::with_open_ask(card());
    let mobile = world.open("mobile");
    let freshness = mobile.refresh(2_000).expect("refresh");
    assert_eq!(freshness.hosts.len(), 1, "{freshness:?}");
    assert_eq!(freshness.hosts[0].host, MAC);
    assert!(!freshness.hosts[0].is_self);
    assert_eq!(
        freshness.hosts[0].last_event_at,
        world.now.add_seconds(-570).canonical()
    );

    let core = world.core();
    let slug = ProjectSlug::parse(PROJECT).expect("slug");
    let project = &core.state.projects[&slug];
    let (_, decision) = find_decision(&core.state, &event(&ask)).expect("decision");
    let revision = decision_revision(&slug, decision, &project.policy.revision);

    let board = mobile.now(None).expect("now");
    assert_eq!(board.needs.len(), 1, "{board:?}");
    let row = &board.needs[0];
    let need = &project.needs_you[0];
    assert_eq!(row.id, need.id);
    assert_eq!(row.kind, NeedKind::Decide);
    assert_eq!(row.source, ask);
    assert_eq!(
        (row.project.as_str(), row.project_title.as_str()),
        (PROJECT, "Mobile")
    );
    assert_eq!(row.node.as_deref(), Some("n0"));
    assert_eq!(row.step_title.as_deref(), Some("First step"));
    assert_eq!(row.question, "How should answers reach idle agents?");
    assert_eq!(row.question_msg_json, None);
    assert!(row.reason.starts_with("1 agent waiting"), "{row:?}");
    let reason: Value =
        serde_json::from_str(row.reason_msg_json.as_deref().expect("reason msg")).expect("json");
    assert_eq!(reason["key"], "need.reason.agents");
    assert_eq!(row.stalled_agents, 1);
    assert_eq!(row.since, need.since.to_string());
    assert_eq!(row.stalled_agents, need.stalled_agents);
    assert_eq!(row.default_at, None);
    assert_eq!(row.guard, Some(Guard::Easy));
    assert_eq!(row.snoozed_until, None);
    assert_eq!(row.revision, revision.as_str());
    assert_eq!(
        row.revision,
        need_revision(need, Some(decision), &project.policy.revision).as_str()
    );
    assert!(board.snoozed.is_empty() && board.done_since.is_empty());

    // The one session is in exactly one list, and the lists agree with the
    // project row.
    let rows = mobile.projects().expect("projects");
    assert_eq!(rows.len(), 1);
    let summary = &project.summary;
    assert_eq!(rows[0].slug, PROJECT);
    assert_eq!(rows[0].title, "Mobile");
    assert_eq!(rows[0].glyph, summary.glyph);
    assert_eq!(rows[0].needs, 1);
    assert_eq!(rows[0].needs, summary.needs_count);
    // Stalled on the ask: counted as stalled only, though still live.
    assert_eq!((summary.active_agents, summary.stalled_agents), (1, 1));
    assert_eq!(rows[0].moving as usize, board.moving.len());
    assert_eq!(rows[0].stalled as usize, board.stalled.len());
    assert_eq!((rows[0].moving, rows[0].stalled), (0, 1));
    assert_eq!(rows[0].stopped_steps, summary.stopped_steps);
    assert_eq!(
        rows[0].last_event,
        summary.last_event.as_ref().map(ToString::to_string)
    );
    assert_eq!(summary.stalled_agents, 1);
    assert!(board.moving.is_empty(), "{board:?}");
    assert_eq!(board.stalled.len(), 1, "{board:?}");
    let session = &board.stalled[0];
    assert_eq!(session.session, SESSION);
    assert_eq!(session.project, PROJECT);
    assert_eq!(session.harness, "generic");
    assert_eq!(session.step_title.as_deref(), Some("First step"));
    assert_eq!(session.since, need.since.to_string());

    let card = mobile.card(ask.clone()).expect("card");
    assert_eq!(card.ask_id, ask);
    assert_eq!(card.status, CardStatus::Open);
    assert_eq!(card.revision, row.revision);
    assert_eq!(
        (card.project.as_str(), card.project_title.as_str()),
        (PROJECT, "Mobile")
    );
    assert_eq!(
        (card.node.as_str(), card.step_title.as_str()),
        ("n0", "First step")
    );
    assert_eq!(
        (card.guard, card.risk, card.external),
        (Guard::Easy, Risk::Easy, false)
    );
    assert!(card.risk_words.is_empty());
    assert_eq!(card.mode, AskMode::Ask);
    assert_eq!(card.why_now, "Wake delivery is blocked.");
    assert_eq!(card.recommended_option, "adapters");
    assert_eq!(card.recommended_confidence, Confidence::Med);
    assert_eq!(card.recommended_because, "It is simpler.");
    assert_eq!(card.own_max_chars, 600);
    assert_eq!(card.options.len(), 2);
    assert_eq!(card.options[0].id, "adapters");
    assert_eq!(card.options[0].label, "Use harness adapters");
    let eta = card.options[0].eta.expect("eta");
    assert_eq!((eta.min_s, eta.max_s), (1800, 3600));
    assert_eq!(card.options[1].cost.as_deref(), Some("One more start"));
    assert_eq!(card.options[1].eta, None);
    assert!(!card.options[1].has_image && !card.options[1].external);
    assert_eq!(card.options[1].risk, Risk::Easy);
    assert_eq!(card.facts.len(), 1);
    assert_eq!(card.facts[0].claim, FactClaim::AgentSays);
    assert_eq!(card.done_when.len(), 1);
    assert_eq!(card.done_when[0].id, "bytes");
    assert_eq!(card.stalled_agents, need.stalled_agents);
    assert_eq!(card.since, need.since.to_string());
    assert_eq!((card.default_at, card.default_option), (None, None));
}

#[test]
fn the_guard_follows_the_core_for_costly_and_external_cards() {
    let mut costly = card();
    costly["options"][1]["reversibility"] = json!("costly");
    let (mut world, choose) = World::with_open_ask(costly);
    let mut external = card();
    external["options"][0]["external"] = json!(true);
    world.step(560, "n1", "Second step", &[]);
    let confirm = world.ask(550, "n1", external, None);
    let mut unsure = card();
    unsure["recommended"]["confidence"] = json!("low");
    world.step(540, "n2", "Third step", &[]);
    let low = world.ask(530, "n2", unsure, None);

    let mobile = world.open("mobile");
    mobile.refresh(2_000).expect("refresh");
    let card = mobile.card(choose.clone()).expect("card");
    assert_eq!(
        (card.guard, card.risk, card.external),
        (Guard::Choose, Risk::Costly, false)
    );
    assert_eq!(card.options[1].risk, Risk::Costly);
    let card = mobile.card(confirm.clone()).expect("card");
    assert_eq!(
        (card.guard, card.risk, card.external),
        (Guard::Confirm, Risk::Irreversible, true)
    );
    // The option keeps its declared reversibility; `external` is beside it.
    assert_eq!(card.options[0].risk, Risk::Easy);
    assert!(card.options[0].external);
    let card = mobile.card(low.clone()).expect("card");
    assert_eq!((card.guard, card.risk), (Guard::Choose, Risk::Easy));
    assert_eq!(card.recommended_confidence, Confidence::Low);

    let guards: BTreeMap<String, Option<Guard>> = mobile
        .now(None)
        .expect("now")
        .needs
        .into_iter()
        .map(|row| (row.source, row.guard))
        .collect();
    assert_eq!(guards[&choose], Some(Guard::Choose));
    assert_eq!(guards[&confirm], Some(Guard::Confirm));
    assert_eq!(guards[&low], Some(Guard::Choose));
}

#[test]
fn an_answer_is_here_on_its_host_and_elsewhere_on_the_others() {
    let (mut world, ask) = World::with_open_ask(card());
    let mobile = world.open("mobile");
    let tablet = world.open("tablet");
    mobile.refresh(2_000).expect("refresh");
    let open_revision = mobile.card(ask.clone()).expect("card").revision;
    let answer = mobile
        .answer(ask.clone(), "inbox".to_owned())
        .expect("answer");
    // The cards read the index: the answer shows after the next refresh.
    mobile.refresh(2_000).expect("refresh");

    let here = mobile.card(ask.clone()).expect("card");
    assert_eq!(
        here.status,
        CardStatus::AnsweredHere {
            label: "Use inbox on next start".to_owned(),
            answer_id: answer.clone(),
        }
    );
    // The receipt is part of the revision, as on the desktop.
    assert_ne!(here.revision, open_revision);
    assert!(mobile.now(None).expect("now").needs.is_empty());
    assert_eq!(mobile.projects().expect("projects")[0].needs, 0);

    let mobile_host = mobile.refresh(2_000).expect("refresh").this_host;
    let seen = tablet.refresh(2_000).expect("refresh");
    assert_ne!(seen.this_host, mobile_host);
    let hosts: Vec<(&str, bool)> = seen
        .hosts
        .iter()
        .map(|host| (host.host.as_str(), host.is_self))
        .collect();
    // Newest first: the mobile client answered after the Mac's last event.
    assert_eq!(hosts, [(mobile_host.as_str(), false), (MAC, false)]);
    let elsewhere = tablet.card(ask.clone()).expect("card");
    assert_eq!(
        elsewhere.status,
        CardStatus::AnsweredElsewhere {
            label: "Use inbox on next start".to_owned(),
            host: mobile_host.clone(),
            answer_id: answer.clone(),
        }
    );
    assert_eq!(elsewhere.revision, here.revision);
    let own = mobile.refresh(2_000).expect("refresh");
    assert!(
        own.hosts
            .iter()
            .any(|host| host.is_self && host.host == mobile_host)
    );

    // The agent's ack keeps the status.
    world.push(
        0,
        &agent_writer(),
        "decision.ack",
        json!({"node": "n0", "ask": ask, "answer": answer}),
        json!({"agent": agent(), "applied_option": "inbox", "restarted": []}),
    );
    mobile.refresh(2_000).expect("refresh");
    let acked = mobile.card(ask.clone()).expect("card");
    assert!(
        matches!(acked.status, CardStatus::AnsweredHere { .. }),
        "{acked:?}"
    );
}

#[test]
fn a_withdrawn_ask_and_a_superseded_ask_say_so() {
    let (mut world, withdrawn) = World::with_open_ask(card());
    world.step(560, "n1", "Second step", &[]);
    let replaced = world.ask(550, "n1", card(), None);
    world.push(
        500,
        &agent_writer(),
        "decision.withdrawn",
        json!({"node": "n0", "ask": withdrawn}),
        json!({"agent": agent(), "reason": "No longer needed."}),
    );
    let newer = world.ask(490, "n1", card(), Some(&replaced));

    let mobile = world.open("mobile");
    mobile.refresh(2_000).expect("refresh");
    assert_eq!(
        mobile.card(withdrawn.clone()).expect("card").status,
        CardStatus::Withdrawn
    );
    assert_eq!(
        mobile.card(replaced.clone()).expect("card").status,
        CardStatus::Superseded
    );
    assert_eq!(
        mobile.card(newer.clone()).expect("card").status,
        CardStatus::Open
    );
    let sources: Vec<String> = mobile
        .now(None)
        .expect("now")
        .needs
        .into_iter()
        .map(|row| row.source)
        .collect();
    assert_eq!(sources, [newer]);
}

#[test]
fn a_timed_out_ask_shows_its_default_then_is_defaulted() {
    let mut timed = card();
    timed["on_timeout"] = json!({"kind": "proceed_as_assumption", "after_s": 120});
    let (mut world, ask) = World::with_open_ask(timed);
    let mobile = world.open("mobile");
    mobile.refresh(2_000).expect("refresh");

    let default_at = world.now.add_seconds(-570 + 120).canonical();
    let open = mobile.card(ask.clone()).expect("card");
    assert_eq!(open.status, CardStatus::Open);
    assert_eq!(open.default_at.as_deref(), Some(default_at.as_str()));
    assert_eq!(open.default_option.as_deref(), Some("adapters"));
    let row = &mobile.now(None).expect("now").needs[0];
    assert_eq!(row.default_at.as_deref(), Some(default_at.as_str()));

    world.push(
        400,
        &agent_writer(),
        "decision.assumed",
        json!({"node": "n0", "ask": ask, "attempt": format!("{:026}", 3)}),
        json!({"agent": agent(), "option": "adapters", "reason": "timeout", "policy": policy(&card())}),
    );
    mobile.refresh(2_000).expect("refresh");
    let defaulted = mobile.card(ask.clone()).expect("card");
    assert_eq!(
        defaulted.status,
        CardStatus::Defaulted {
            label: "Use harness adapters".to_owned(),
            at: world.now.add_seconds(-400).canonical(),
        },
        "{:?}",
        world.core().state.diagnostics
    );
    assert_eq!(
        (defaulted.default_at, defaulted.default_option),
        (None, None)
    );
}

#[test]
fn a_rescue_row_has_no_guard_and_the_need_revision() {
    let (mut world, ask) = World::with_open_ask(card());
    // A dependency cycle the reducer rejects: an active rescue row.
    world.step(300, "n1", "Second step", &["n2"]);
    world.step(290, "n2", "Third step", &["n1"]);
    let mobile = world.open("mobile");
    mobile.refresh(2_000).expect("refresh");

    let core = world.core();
    let expected = core_needs(&core);
    let board = mobile.now(None).expect("now");
    let ids: Vec<&str> = board.needs.iter().map(|row| row.id.as_str()).collect();
    let expected_ids: Vec<&str> = expected.iter().map(|need| need.id.as_str()).collect();
    assert_eq!(ids, expected_ids);
    assert_eq!(board.needs.len(), 2, "{board:?}");

    let slug = ProjectSlug::parse(PROJECT).expect("slug");
    let policy = &core.state.projects[&slug].policy.revision;
    let need = expected
        .iter()
        .find(|need| need.id.starts_with("rescue:cycle:"))
        .expect("a cycle row");
    let row = board
        .needs
        .iter()
        .find(|row| row.id == need.id)
        .expect("rescue row");
    assert_eq!(row.kind, NeedKind::Rescue);
    assert_eq!(row.guard, None);
    assert_eq!(row.default_at, None);
    assert_eq!(row.revision, need_revision(need, None, policy).as_str());
    assert_eq!(row.node, need.node.as_ref().map(ToString::to_string));
    assert_eq!(row.question, need.question);
    let question: Value =
        serde_json::from_str(row.question_msg_json.as_deref().expect("question msg"))
            .expect("json");
    assert_eq!(question["key"], "need.q.cycle");
    // A rescue row has no card.
    assert_eq!(
        core_code(mobile.card(row.source.clone()).expect_err("no card")),
        "E_NOT_FOUND"
    );
    assert!(mobile.card(ask).is_ok());

    // Nobody snoozed anything in this Vault (`tests/snooze.rs` does).
    assert!(board.snoozed.is_empty());
    assert!(board.needs.iter().all(|row| row.snoozed_until.is_none()));
    let page = mobile.project(PROJECT.to_owned()).expect("page");
    assert!(page.needs.iter().all(|row| row.snoozed_until.is_none()));
    let listed = |rows: &[agendar_ffi::NeedRow]| -> Vec<(String, String)> {
        let pair = |row: &agendar_ffi::NeedRow| (row.id.clone(), row.revision.clone());
        rows.iter().map(pair).collect()
    };
    assert_eq!(listed(&page.needs), listed(&board.needs));
}

#[test]
fn a_project_page_lists_the_plan_in_order_with_the_desktop_forecast() {
    let mut world = World::new();
    world.declare(40_000);
    // Three steps finished after an hour or more each, one in progress, one
    // not started. `a-last` depends on `z-first`, so it comes after it.
    for (index, node) in ["d1", "d2", "d3"].into_iter().enumerate() {
        let age = 30_000 - 8_000 * index as i64;
        world.step(age + 10, node, &format!("Done {node}"), &[]);
        let attempt = world.start(age, node);
        world.push(
            age - 3_600 - 600 * index as i64,
            &agent_writer(),
            "work.progress",
            json!({"node": node, "attempt": attempt}),
            json!({"agent": agent(), "evidence": [], "status": "done"}),
        );
    }
    world.push(
        900,
        &operator(),
        "plan.feature",
        json!({"node": "z-first"}),
        json!({"demo_kind": "cli_transcript", "depends_on": [], "slice": "s1",
               "title": "In progress", "milestone": "Beta"}),
    );
    world.step(890, "a-last", "Not started", &["z-first"]);
    world.start(120, "z-first");

    let mobile = world.open("mobile");
    mobile.refresh(2_000).expect("refresh");
    let page = mobile.project(PROJECT.to_owned()).expect("page");
    assert_eq!(page.row, mobile.projects().expect("projects")[0]);
    assert_eq!(page.goal, "Answer from a mobile client");
    assert_eq!(page.brief, "Fixture");
    assert!(page.needs.is_empty());

    let nodes: Vec<&str> = page.steps.iter().map(|step| step.node.as_str()).collect();
    assert_eq!(nodes, ["d1", "d2", "d3", "z-first", "a-last"]);
    let doing = &page.steps[3];
    assert_eq!(doing.title, "In progress");
    assert_eq!(doing.progress, StepProgress::Doing);
    assert_eq!(doing.milestone.as_deref(), Some("Beta"));
    assert_eq!(doing.holder.as_deref(), Some(SESSION));
    assert!(!doing.paused && !doing.dropped_by_owner);
    let window = doing.expected_finish.as_ref().expect("window");
    assert!(window.earliest <= window.latest, "{window:?}");
    assert_eq!(page.steps[0].progress, StepProgress::Done);
    assert_eq!(page.steps[0].holder, None);
    let planned = &page.steps[4];
    assert_eq!(planned.progress, StepProgress::Planned);
    assert_eq!(
        (planned.holder.as_ref(), planned.expected_finish.as_ref()),
        (None, None)
    );

    // The forecast is the one the desktop's statistics build.
    let core = world.core();
    let project = &core.state.projects[&ProjectSlug::parse(PROJECT).expect("slug")];
    let expected = stats::build(project, &core.events, core.now)
        .forecast
        .expect("the core forecasts");
    let forecast = page.forecast.expect("forecast");
    assert_eq!(forecast.open_steps, 2);
    assert_eq!(forecast.open_steps, expected.open_steps);
    assert_eq!(forecast.in_progress, expected.in_progress);
    assert_eq!(forecast.abandoned, expected.abandoned);
    assert_eq!(forecast.not_started, expected.not_started);
    assert_eq!(forecast.agents, expected.agents);
    assert_eq!(forecast.basis_steps, 3);
    assert_eq!(forecast.basis_steps, expected.basis_steps);
    assert!(forecast.work_low_ms <= forecast.work_high_ms);
    assert_eq!(forecast.finish.is_some(), expected.finish.is_some());
    assert_eq!(forecast.agents, 1);
    assert!(forecast.finish.is_some());

    let moving = mobile.now(None).expect("now").moving;
    assert_eq!(moving.len(), 1, "{moving:?}");
    assert_eq!(moving[0].step_title.as_deref(), Some("In progress"));
    assert_eq!(moving[0].since, world.now.add_seconds(-120).canonical());
}

#[test]
fn a_project_page_carries_the_desktop_activity_and_header_numbers() {
    let mut world = World::new();
    world.declare(40_000);
    world.push(
        30_000,
        &operator(),
        "plan.feature",
        json!({"node": "n0"}),
        json!({"demo_kind": "cli_transcript", "depends_on": [], "slice": "s0",
               "title": "First step", "milestone": "Beta", "priority": 2,
               "deadline": {"op": "set", "value": "2030-01-01T00:00:00.000Z"}}),
    );
    world.step(29_940, "n1", "Second step", &["n0"]);
    // A step finished in 90 minutes inside the last day, then a question the
    // owner answers, then a progress update.
    let first = world.start(20_000, "n0");
    world.push(
        14_600,
        &agent_writer(),
        "work.progress",
        json!({"node": "n0", "attempt": first}),
        json!({"agent": agent(), "evidence": [], "status": "done"}),
    );
    let second = world.start(9_000, "n1");
    let ask = world.ask(8_000, "n1", card(), None);
    world.push(
        7_000,
        &operator(),
        "decision.answer",
        json!({"node": "n1", "ask": ask}),
        json!({"option": "adapters", "done_when": [{"id": "bytes", "text": "Demo bytes match."}]}),
    );
    world.push(
        600,
        &agent_writer(),
        "pulse",
        json!({"node": "n1", "attempt": second}),
        json!({"agent": agent(), "message": "Wiring the adapters", "boundary": false}),
    );

    let mobile = world.open("mobile");
    mobile.refresh(2_000).expect("refresh");
    let page = mobile.project(PROJECT.to_owned()).expect("page");

    // The feed and the numbers are the ones the desktop host builds.
    let core = world.core();
    let slug = ProjectSlug::parse(PROJECT).expect("slug");
    let project = &core.state.projects[&slug];
    let events =
        recent_project_events(&core.index, &slug, activity::ACTIVITY_EVENT_WINDOW).expect("events");
    let feed = activity::build(project, &events);
    assert!(!feed.is_empty());
    assert_eq!(page.activity.len(), feed.len());
    for (row, item) in page.activity.iter().zip(&feed) {
        assert_eq!(row.at, item.ts.to_string());
        assert_eq!(row.text, item.text);
        assert_eq!(
            row.msg_json
                .as_deref()
                .map(|msg| serde_json::from_str::<Value>(msg).expect("json")),
            item.msg
                .as_ref()
                .map(|msg| serde_json::to_value(msg).expect("json"))
        );
        assert_eq!(row.detail, item.detail);
        assert_eq!(
            row.node.as_deref(),
            item.node.as_ref().map(|node| node.as_str())
        );
        assert_eq!(row.actor, item.actor);
        assert_eq!(row.events, item.count);
        assert_eq!(row.by_owner, item.actor_kind == activity::ActorKind::Human);
        assert_ne!(row.kind, ActivityKind::Other, "{row:?}");
    }
    let times: Vec<&str> = page.activity.iter().map(|row| row.at.as_str()).collect();
    assert!(
        times.windows(2).all(|pair| pair[0] >= pair[1]),
        "newest first: {times:?}"
    );

    let kinds: Vec<ActivityKind> = page.activity.iter().map(|row| row.kind).collect();
    assert_eq!(
        kinds,
        [
            ActivityKind::Update,
            ActivityKind::Answered,
            ActivityKind::Asked,
            ActivityKind::Started,
            ActivityKind::Finished,
            ActivityKind::Started,
            ActivityKind::PlanUpdated,
            ActivityKind::ProjectDescribed,
        ]
    );
    let update = &page.activity[0];
    assert_eq!(update.text, "Agent: Wiring the adapters");
    assert_eq!(update.node.as_deref(), Some("n1"));
    assert_eq!(update.step_title.as_deref(), Some("Second step"));
    assert_eq!(update.harness.as_deref(), Some("generic"));
    assert!(!update.by_owner && !update.has_report);
    let answered = &page.activity[1];
    assert!(answered.by_owner);
    assert_eq!(
        (answered.actor.as_str(), answered.harness.as_ref()),
        ("You", None)
    );
    assert!(
        answered.text.contains("Use harness adapters"),
        "{}",
        answered.text
    );
    let plan = &page.activity[6];
    assert!(plan.by_owner);
    assert_eq!(plan.events, 2, "two plan updates in one burst");
    assert_eq!(plan.node, None, "a burst about two steps names none");

    let expected = stats::build(project, &events, core.now);
    assert_eq!(page.stats.agents_running, expected.agents_running);
    assert_eq!(page.stats.agents_idle, expected.agents_idle);
    assert_eq!(page.stats.agents_stalled, expected.agents_stalled);
    assert_eq!(page.stats.finished_24h, expected.finished_24h);
    assert_eq!(page.stats.agent_ms_24h, expected.agent_ms_24h);
    assert_eq!(page.stats.median_step_ms, expected.median_step_ms);
    assert_eq!(page.stats.timed_steps, expected.timed_steps);
    assert_eq!(page.stats.instant_excluded, expected.instant_excluded);
    assert_eq!((page.stats.agents_running, page.stats.finished_24h), (1, 1));
    assert_eq!(page.stats.agent_ms_24h, 5_400_000);
    assert_eq!(page.stats.median_step_ms, Some(5_400_000));
    assert_eq!(page.calibration.len(), project.summary.calibration.len());
    for (row, core_row) in page.calibration.iter().zip(&project.summary.calibration) {
        assert_eq!(
            (row.samples, row.ratio_milli),
            (core_row.samples, core_row.ratio_milli)
        );
    }
    assert!(page.deliveries.is_empty());

    // The step fields the desktop's plan shows.
    let first = &page.steps[0];
    assert_eq!(first.milestone.as_deref(), Some("Beta"));
    assert_eq!(first.priority, 2);
    assert_eq!(first.deadline.as_deref(), Some("2030-01-01T00:00:00.000Z"));
    assert_eq!((first.gate, first.phase), (StepGate::Open, None));
    assert!(first.depends_on.is_empty());
    assert_eq!(page.steps[1].depends_on, ["n0"]);
    assert_eq!(
        page.steps[1].late_by_s,
        project.features[&NodePath::parse("n1").expect("node")]
            .eta
            .as_ref()
            .expect("eta")
            .late_by_s
    );
}

#[test]
fn the_activity_is_capped_at_forty_lines_newest_first() {
    let mut world = World::new();
    world.declare(100_000);
    world.step(99_000, "n0", "First step", &[]);
    let attempt = world.start(98_000, "n0");
    // Sixty updates ten minutes apart: none folds into its neighbour.
    for index in 0..60 {
        world.push(
            36_000 - 600 * index,
            &agent_writer(),
            "pulse",
            json!({"node": "n0", "attempt": attempt}),
            json!({"agent": agent(), "message": format!("Update {index}"), "boundary": false}),
        );
    }
    let mobile = world.open("mobile");
    mobile.refresh(2_000).expect("refresh");
    let activity = mobile.project(PROJECT.to_owned()).expect("page").activity;
    assert_eq!(activity.len(), activity::ACTIVITY_MAX_ITEMS);
    assert_eq!(activity.len(), 40);
    assert_eq!(activity[0].text, "Agent: Update 59");
    assert_eq!(activity[39].text, "Agent: Update 20");
    assert!(activity.iter().all(|row| row.kind == ActivityKind::Update));
}

#[test]
fn projects_come_in_the_core_order_without_archived_ones() {
    let mut world = World::new();
    for (slug, title) in [("alpha", "Alpha"), ("beta", "Beta"), ("gamma", "Gamma")] {
        world.push_in(
            slug,
            900,
            &operator(),
            "project.declared",
            json!({}),
            json!({"brief": "Fixture", "goal": "Order", "title": title}),
        );
    }
    // Only `beta` needs the owner: it sorts before its quiet neighbours.
    for (seconds, node) in [(800, "n0"), (700, "n1")] {
        world.push_in(
            "beta",
            seconds,
            &operator(),
            "plan.feature",
            json!({"node": node}),
            json!({"demo_kind": "cli_transcript", "depends_on": [if node == "n0" { "n1" } else { "n0" }],
                   "slice": "s0", "title": node}),
        );
    }
    world.push_in(
        "gamma",
        600,
        &operator(),
        "project.declared",
        json!({}),
        json!({"archived": true}),
    );
    let mobile = world.open("mobile");
    mobile.refresh(2_000).expect("refresh");

    let core = world.core();
    let needs = core_needs(&core);
    let earliest = ordering::earliest_needs(&needs);
    let mut expected: Vec<_> = core
        .state
        .projects
        .values()
        .map(|project| project.summary.clone())
        .filter(|summary| !summary.archived)
        .collect();
    ordering::sort_projects(&mut expected, &earliest);
    let expected: Vec<&str> = expected
        .iter()
        .map(|summary| summary.slug.as_str())
        .collect();

    let rows = mobile.projects().expect("projects");
    let slugs: Vec<&str> = rows.iter().map(|row| row.slug.as_str()).collect();
    assert_eq!(slugs, expected);
    assert_eq!(slugs, ["beta", "alpha"], "{:?}", core.state.diagnostics);
    assert_eq!(rows[0].needs, 1);
    // An archived project still has a page, and is on no list.
    assert_eq!(
        mobile.project("gamma".to_owned()).expect("page").row.slug,
        "gamma"
    );
}

/// The workload the iOS fixture Vault is generated from, at a size a test
/// can afford: 120 features from three agent hosts, one open ask.
fn generated(vault: &Path, now: UtcTime) -> usize {
    let shape = |base: UtcTime| workload::Shape {
        features: 120,
        deep: false,
        agent_hosts: 3,
        open_ask: Some(100),
        base,
    };
    // The last event is ten minutes old, as in the fixture.
    let (dry, as_of) = workload::workload(&shape(now));
    let span = (as_of.millis() - now.millis()) / 1000;
    let (events, _) = workload::workload(&shape(now.add_seconds(-span)));
    assert_eq!(dry.len(), events.len());
    let vault = Vault::open(vault).expect("vault");
    for event in &events {
        support::append_event_to_vault(&vault, event, 1).expect("append event");
    }
    events.len()
}

#[test]
fn the_generated_fixture_reads_like_the_desktop() {
    let world = World::new();
    let events = generated(&world.vault(), world.now);
    let mobile = world.open("mobile");
    let freshness = mobile.refresh(5_000).expect("refresh");
    assert_eq!(freshness.events as usize, events);
    assert!(freshness.event_set_digest.is_some() && freshness.waiting.is_empty());
    let mut hosts: Vec<String> = freshness
        .hosts
        .iter()
        .map(|host| host.host.clone())
        .collect();
    assert!(
        freshness
            .hosts
            .windows(2)
            .all(|pair| pair[0].last_event_at >= pair[1].last_event_at),
        "{freshness:?}"
    );
    hosts.sort();
    assert_eq!(
        hosts,
        [workload::host(0), workload::host(1), workload::host(2)]
    );

    let core = world.core();
    assert_eq!(
        freshness.event_set_digest.as_deref(),
        Some(core.state.event_set_digest.as_str())
    );
    let slug = ProjectSlug::parse(workload::PROJECT).expect("slug");
    let project = &core.state.projects[&slug];
    let policy = &project.policy.revision;

    // Needs you: the core's active rows, in the core's order, with the
    // desktop's revisions.
    let expected = core_needs(&core);
    let board = mobile.now(None).expect("now");
    assert_eq!(board.needs.len(), expected.len());
    assert!(
        expected
            .iter()
            .filter(|need| need.kind.as_str() == "decide")
            .count()
            == 1,
        "one open ask"
    );
    for (row, need) in board.needs.iter().zip(&expected) {
        assert_eq!(row.id, need.id);
        let decision = project.decisions.get(&need.source);
        assert_eq!(row.revision, need_revision(need, decision, policy).as_str());
        assert!(row.waiting_minutes.abs_diff(need.waiting_minutes) <= 1);
        assert_eq!(row.guard.is_some(), need.kind.as_str() == "decide");
    }
    assert!(project.needs_you.len() >= expected.len());
    assert!(board.snoozed.is_empty() && board.done_since.is_empty());

    let rows = mobile.projects().expect("projects");
    assert_eq!(rows.len(), 1);
    let summary = &project.summary;
    assert_eq!(rows[0].needs as usize, expected.len());
    assert_eq!(
        (
            rows[0].needs,
            rows[0].moving,
            rows[0].stalled,
            rows[0].stopped_steps
        ),
        (
            summary.needs_count,
            summary.active_agents,
            summary.stalled_agents,
            summary.stopped_steps
        )
    );
    assert_eq!(board.stalled.len(), summary.stalled_agents as usize);
    assert!(board.moving.len() <= summary.active_agents as usize);

    // Every card of the project opens, with the desktop's revision.
    for (id, decision) in &project.decisions {
        let card = mobile.card(id.as_str().to_owned()).expect("card");
        assert_eq!(
            card.revision,
            decision_revision(&slug, decision, policy).as_str()
        );
        let open = decision.lifecycle == agendar_core::model::DecisionLifecycle::Open;
        assert_eq!(card.status == CardStatus::Open, open, "{id}");
        if !open {
            // The Mac's operator answered them all.
            assert!(
                matches!(&card.status, CardStatus::AnsweredElsewhere { host, .. } if host == MAC),
                "{card:?}"
            );
        }
    }

    // The page: every step once, each after the steps it depends on unless
    // the plan has them in a cycle.
    let page = mobile.project(workload::PROJECT.to_owned()).expect("page");
    assert_eq!(page.steps.len(), project.features.len());
    let listed = |rows: &[agendar_ffi::NeedRow]| -> Vec<(String, String)> {
        let pair = |row: &agendar_ffi::NeedRow| (row.id.clone(), row.revision.clone());
        rows.iter().map(pair).collect()
    };
    assert_eq!(listed(&page.needs), listed(&board.needs));
    let position: BTreeMap<&str, usize> = page
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| (step.node.as_str(), index))
        .collect();
    assert_eq!(position.len(), page.steps.len());
    let late: Vec<&str> = project
        .features
        .values()
        .filter(|feature| {
            feature.depends_on.iter().any(|dependency| {
                let mutual = project.features[dependency]
                    .depends_on
                    .contains(&feature.node);
                !mutual && position[dependency.as_str()] > position[feature.node.as_str()]
            })
        })
        .map(|feature| feature.node.as_str())
        .collect();
    assert!(late.is_empty(), "{late:?}");

    // Done since: the brief's Delivered rows after the mark, newest first.
    let mark = world.now.add_seconds(-86_400);
    let delivered = brief::build(&core.state, &core.events, Some(mark)).delivered;
    assert!(!delivered.is_empty());
    let done = mobile.now(Some(mark.canonical())).expect("now").done_since;
    let ids: Vec<&str> = done.iter().map(|row| row.id.as_str()).collect();
    let expected_ids: Vec<&str> = delivered.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, expected_ids);
    assert_eq!(done[0].text, delivered[0].text);
    assert_eq!(done[0].project, workload::PROJECT);
    assert!(done.windows(2).all(|pair| pair[0].at >= pair[1].at));
    assert!(done.iter().all(|row| row.at > mark.canonical()));
    // A mark without a fraction of a second is accepted, and nothing is
    // newer than now.
    let whole = format!("{}Z", &world.now.add_seconds(60).canonical()[..19]);
    assert!(mobile.now(Some(whole)).expect("now").done_since.is_empty());
}

#[test]
fn a_file_that_is_not_downloaded_is_waiting() {
    let (world, ask) = World::with_open_ask(card());
    let mobile = world.open("mobile");
    mobile.refresh(2_000).expect("refresh");
    let stream = world
        .vault()
        .join("projects")
        .join(PROJECT)
        .join(MAC)
        .join(format!("s-{SESSION}.jsonl"));
    std::fs::rename(
        &stream,
        stream.with_file_name(format!(".s-{SESSION}.jsonl.icloud")),
    )
    .expect("evict");
    let freshness = mobile.refresh(2_000).expect("refresh");
    assert_eq!(
        freshness.waiting,
        [format!("projects/{PROJECT}/{MAC}/s-{SESSION}.jsonl")]
    );
    // What was read stays.
    assert_eq!(freshness.events, 4);
    assert_eq!(mobile.card(ask).expect("card").status, CardStatus::Open);
}

#[test]
fn a_refresh_that_cannot_take_the_index_lock_in_time_is_refused() {
    let (mut world, _) = World::with_open_ask(card());
    let mobile = world.open("mobile");
    assert_eq!(mobile.refresh(2_000).expect("refresh").events, 4);

    // Another process of this device holds the ingest lock while the Vault
    // has a new line to read.
    world.step(10, "n1", "Second step", &[]);
    let other = Service::open(&world.vault(), &world.dir.path().join("mobile")).expect("service");
    let index = agendar_core::index::open_index(other.local(), other.vault()).expect("index");
    let lock = index.acquire_ingest_lock(other.vault()).expect("lock");
    let refused = mobile.refresh(50).expect_err("the lock is held");
    let FfiError::Refused { code, .. } = refused else {
        panic!("a refusal, not {refused}");
    };
    assert_eq!(code, RefusalCode::RefreshTimeout);
    // The cached reads still answer with what was read before.
    assert_eq!(
        mobile
            .project(PROJECT.to_owned())
            .expect("page")
            .steps
            .len(),
        1
    );

    drop(lock);
    assert_eq!(mobile.refresh(0).expect("refresh").events, 5);
}

#[test]
fn an_assumption_the_owner_may_still_replace_is_defaulted() {
    // A costly local choice: the balanced policy lets the agent assume.
    let mut costly = card();
    costly["options"][1]["reversibility"] = json!("costly");
    let mut assume = policy(&costly);
    assume["effective"] = json!("assume");
    assume["requested"] = json!("assume");
    let mut world = World::new();
    world.declare(600);
    world.step(590, "n0", "First step", &[]);
    let attempt = world.start(580, "n0");
    let ask = world.push(
        570,
        &agent_writer(),
        "decision.ask",
        json!({"node": "n0"}),
        json!({"agent": agent(), "card": costly, "policy": assume}),
    );
    world.push(
        400,
        &agent_writer(),
        "decision.assumed",
        json!({"node": "n0", "ask": ask, "attempt": attempt}),
        json!({"agent": agent(), "option": "adapters", "reason": "policy", "policy": assume}),
    );
    let mobile = world.open("mobile");
    mobile.refresh(2_000).expect("refresh");
    let core = world.core();
    let (_, decision) = find_decision(&core.state, &event(&ask)).expect("decision");
    // The core keeps the ask open and answerable.
    assert_eq!(
        decision.lifecycle,
        agendar_core::model::DecisionLifecycle::Open,
        "{:?}",
        core.state.diagnostics
    );
    assert!(
        decision.effective_receipt.is_some(),
        "{:?}",
        core.state.diagnostics
    );
    assert_eq!(
        mobile.card(ask).expect("card").status,
        CardStatus::Defaulted {
            label: "Use harness adapters".to_owned(),
            at: world.now.add_seconds(-400).canonical(),
        }
    );
}
