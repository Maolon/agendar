#![forbid(unsafe_code)]

//! Moving and stalled on a mobile client's Now (`agendar_core::reduce::motion`). A
//! session is moving only on a step in progress it holds: a step delivered
//! and waiting for the owner's review is the owner's, an accepted one is
//! finished. A sent-back step nobody is on is stalled, as on the desktop. The
//! Mac is a core `Service` on the same Vault; the agent writes raw lines, as
//! its host would.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use agendar_core::model::{
    FeatureState, Lease, Liveness, NodePath, Phase, Progress, ProjectSlug, ProjectState, SessionId,
    UtcTime,
};
use agendar_core::reduce::motion::{self, Motion, Stall};
use agendar_core::service::Service;
use agendar_core::vault::{Clock, SystemClock};
use agendar_ffi::{NeedKind, NowBoard, ProjectRow, Session, StallReason, Verdict};
use serde_json::{Value, json};

const PROJECT: &str = "mobile";
const MAC: &str = "h-00000000000000000000000001";
const SESSION: &str = "00000000000000000000000101";

struct World {
    dir: tempfile::TempDir,
    mac: Service,
    mobile: Session,
    next_id: Cell<u64>,
    /// How long ago the next raw line is written, in seconds.
    ago: Cell<i64>,
    seqs: RefCell<BTreeMap<String, u64>>,
}

impl World {
    /// One project with the planned steps `steps` (node, title), all
    /// independent.
    fn new(steps: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = dir.path().join("vault");
        let (mac, _) = Service::init(&vault, &dir.path().join("mac")).expect("init");
        let mobile = Session::open(
            vault.display().to_string(),
            dir.path().join("mobile").display().to_string(),
        )
        .expect("open");
        let world = Self {
            dir,
            mac,
            mobile,
            next_id: Cell::new(0),
            ago: Cell::new(600),
            seqs: RefCell::new(BTreeMap::new()),
        };
        world.line(
            "operator",
            "project.declared",
            json!({}),
            json!({"brief": "Fixture", "goal": "Who is moving", "title": "Mobile"}),
        );
        for (node, title) in steps {
            world.line(
                "operator",
                "plan.feature",
                json!({"node": node}),
                json!({"demo_kind": "screenshot", "depends_on": [], "slice": "s0", "title": title}),
            );
        }
        world
    }

    /// Append one raw line as a writer of the Mac; returns the event id.
    fn line(&self, writer: &str, kind: &str, refs: Value, body: Value) -> String {
        self.next_id.set(self.next_id.get() + 1);
        let number = self.next_id.get();
        let id = format!("{number:026}");
        let seq = {
            let mut seqs = self.seqs.borrow_mut();
            let seq = seqs.entry(writer.to_owned()).or_insert(0);
            *seq += 1;
            *seq
        };
        let line = json!({
            "v": 1, "id": id, "seq": seq, "writer": format!("{MAC}/{writer}"),
            "ts": SystemClock.now().add_seconds(i64::try_from(number).expect("id") - self.ago.get()).canonical(),
            "project": PROJECT, "refs": refs, "kind": kind, "body": body,
        });
        let host_dir = self
            .dir
            .path()
            .join("vault")
            .join("projects")
            .join(PROJECT)
            .join(MAC);
        std::fs::create_dir_all(&host_dir).expect("host dir");
        let file = host_dir.join(format!("{writer}.jsonl"));
        let mut text = std::fs::read_to_string(&file).unwrap_or_default();
        text.push_str(&serde_json::to_string(&line).expect("json"));
        text.push('\n');
        std::fs::write(&file, text).expect("write line");
        id
    }

    fn agent_line(&self, kind: &str, refs: Value, mut body: Value) -> String {
        body["agent"] = json!({"harness": "claude_code", "session": SESSION});
        self.line(&format!("s-{SESSION}"), kind, refs, body)
    }

    /// The agent starts `node`; returns the attempt.
    fn start(&self, node: &str) -> String {
        self.agent_line(
            "work.progress",
            json!({"node": node}),
            json!({"eta": {"max_s": 900, "min_s": 300}, "evidence": [], "status": "started"}),
        )
    }

    fn deliver(&self, node: &str, attempt: &str) -> String {
        self.agent_line(
            "delivery",
            json!({"node": node, "attempt": attempt}),
            json!({
                "name": "Settings screen",
                "demos": [{"kind": "preview_url", "body": {"url": "http://localhost:3000/"}}],
                "criteria": [],
                "disclosure": {"proven": ["It renders."], "not_proven_yet": [], "faked_for_now": []},
            }),
        )
    }

    /// The agent sets a new ETA on `node`: a work signal.
    fn pulse(&self, node: &str, attempt: &str) -> String {
        self.agent_line(
            "pulse",
            json!({"node": node, "attempt": attempt}),
            json!({"boundary": false, "eta": {"max_s": 900, "min_s": 300}, "message": "On it."}),
        )
    }

    /// The owner's verdict on `delivery`, from the mobile client.
    fn review(&self, delivery: &str, verdict: Verdict) {
        self.mobile.refresh(2_000).expect("refresh");
        let read = self.mobile.delivery(delivery.to_owned()).expect("delivery");
        self.mobile
            .review(delivery.to_owned(), read.revision, verdict)
            .expect("review");
    }

    /// The mobile client's Now and its one project row, after a refresh.
    fn mobile_reads(&self) -> (NowBoard, ProjectRow) {
        self.mobile.refresh(2_000).expect("refresh");
        let board = self.mobile.now(None).expect("now");
        let mut rows = self.mobile.projects().expect("projects");
        assert_eq!(rows.len(), 1);
        (board, rows.remove(0))
    }

    /// The project as the Mac's core reduces it now: what agents are told from.
    fn core(&self) -> ProjectState {
        let fresh = self.mac.refresh(SystemClock.now(), false).expect("refresh");
        fresh.state.projects[&slug()].clone()
    }
}

fn slug() -> ProjectSlug {
    ProjectSlug::parse(PROJECT).expect("slug")
}

fn session() -> SessionId {
    SessionId::parse(SESSION).expect("session")
}

fn step<'a>(core: &'a ProjectState, title: &str) -> &'a FeatureState {
    let found = core.features.values().find(|step| step.title == title);
    found.expect("step")
}

/// When the owner sent `delivery` back, as the core read it.
fn sent_back_at(core: &ProjectState, delivery: &str) -> UtcTime {
    let delivery = core.deliveries.values().find(|d| d.id.as_str() == delivery);
    delivery
        .and_then(|d| d.sent_back.as_ref())
        .expect("sent back")
        .at
}

/// What an agent is told from: the session, the summary's agents, what the
/// session owes and its nudges.
fn agent_facts(world: &World) -> String {
    let core = world.core();
    let owed = world.mac.owed(&slug(), &session(), false).expect("owed");
    let nudges = world.mac.nudges(&slug(), &session(), false);
    format!(
        "{:?} {:?} {} {} {:?} {:?}",
        core.sessions[&session()],
        core.needs_you,
        core.summary.active_agents,
        core.summary.stalled_agents,
        owed,
        nudges.expect("nudges"),
    )
}

fn send_back() -> Verdict {
    Verdict::SendBack {
        reason: Some("The title is cut off.".to_owned()),
        comments: Vec::new(),
    }
}

/// The fault as it was seen: the agent delivered, another step is still open
/// to it, so the core does not stall it, and it was listed as moving.
#[test]
fn a_step_waiting_for_review_is_not_moving() {
    let world = World::new(&[("shots", "Screens"), ("links", "Links")]);
    let attempt = world.start("shots");

    let (board, row) = world.mobile_reads();
    assert_eq!(board.moving.len(), 1, "at work before the delivery");
    assert_eq!(board.moving[0].step_title.as_deref(), Some("Screens"));
    assert_eq!((row.moving, row.stalled), (1, 0));

    world.deliver("shots", &attempt);
    let (board, row) = world.mobile_reads();
    assert_eq!(board.needs.len(), 1);
    assert_eq!(board.needs[0].kind, NeedKind::Review);
    assert!(board.moving.is_empty(), "{:?}", board.moving);
    assert!(board.stalled.is_empty(), "{:?}", board.stalled);
    assert_eq!((row.needs, row.moving, row.stalled), (1, 0, 0));

    // The core still has the session live and not stalled: only the lists of
    // the owner's screen changed.
    let core = world.core();
    assert_eq!(core.sessions[&session()].liveness, Liveness::Live);
    assert!(core.sessions[&session()].stalled_on.is_empty());
    assert_eq!(
        (core.summary.active_agents, core.summary.stalled_agents),
        (1, 0)
    );
}

/// What agents read stays as it was: the state they are told from, what the
/// session owes and its nudges.
#[test]
fn what_agents_read_about_a_step_in_review_is_unchanged() {
    let world = World::new(&[("shots", "Screens")]);
    let attempt = world.start("shots");
    world.deliver("shots", &attempt);

    let (board, row) = world.mobile_reads();
    assert!(board.moving.is_empty() && board.stalled.is_empty());
    assert_eq!((row.needs, row.moving, row.stalled), (1, 0, 0));

    let core = world.core();
    assert_eq!(core.summary.active_agents, 1, "the summary still counts it");
    assert_eq!(core.sessions[&session()].liveness, Liveness::Live);
    let step = core.features.values().next().expect("step");
    assert_eq!(step.progress, Progress::Doing);
    assert_eq!(step.phase, Some(Phase::InReview));
    assert_eq!(step.session.as_ref(), Some(&session()));
    // One delivery in review, nothing owed, no nudge.
    let owed = world.mac.owed(&slug(), &session(), false).expect("owed");
    assert_eq!(owed.in_review, 1);
    assert!(owed.yours.is_empty() && owed.take.is_empty());
    let nudges = world.mac.nudges(&slug(), &session(), false);
    assert!(nudges.expect("nudges").is_empty());
}

/// The core stalls a session on a review row when the session has nothing it
/// may run (its step in review is on hold, or the row is upstream of its
/// step). Stalled on the review of its own current step it is in neither
/// list; stalled on the review of another step it is stalled as before. The
/// stall is set on a reduced state here: the rule reads nothing else.
#[test]
fn a_stall_on_the_review_of_its_own_step_is_not_shown_and_another_is() {
    let world = World::new(&[("shots", "Screens"), ("links", "Links")]);
    let attempt = world.start("shots");
    let delivery = world.deliver("shots", &attempt);
    let mut core = world.core();
    let review_row = core.needs_you[0].clone();
    assert_eq!(review_row.id, format!("review:{delivery}"));

    let mut stalled = core.sessions[&session()].clone();
    stalled.stalled_on = vec![review_row.id.clone()];
    assert_eq!(motion::of_session(&core, &stalled), Motion::Neither);
    core.sessions.insert(session(), stalled.clone());
    assert_eq!(motion::counts(&core), (0, 0));

    // On another step, waiting for that review: stalled since the row.
    stalled.current_node = Some(NodePath::parse("links").expect("node"));
    let links = core.features.values().find(|step| step.title == "Links");
    assert_eq!(
        motion::of_session(&core, &stalled),
        Motion::Stalled {
            step: links,
            since: review_row.since,
            why: Stall::NeedsYou,
        }
    );
    core.sessions.insert(session(), stalled);
    // Live and stalled: counted where its row is, not in both numbers.
    assert_eq!(motion::counts(&core), (0, 1));
}

/// Accepted and nothing else held: the session is live but not moving on a
/// finished step. It is back with the next step it starts.
#[test]
fn an_agent_whose_step_was_accepted_is_not_moving() {
    let world = World::new(&[("shots", "Screens"), ("links", "Links")]);
    let attempt = world.start("shots");
    let delivery = world.deliver("shots", &attempt);
    world.review(&delivery, Verdict::Accept);

    let (board, row) = world.mobile_reads();
    assert!(board.needs.is_empty());
    assert!(board.moving.is_empty(), "{:?}", board.moving);
    assert!(board.stalled.is_empty(), "{:?}", board.stalled);
    assert_eq!((row.moving, row.stalled), (0, 0));

    // What agents are told from still has the session live, on that step.
    let core = world.core();
    let agent = &core.sessions[&session()];
    assert_eq!(agent.liveness, Liveness::Live);
    assert!(agent.stalled_on.is_empty());
    assert_eq!(
        agent.current_node.as_ref().map(NodePath::as_str),
        Some("shots")
    );
    assert!(step(&core, "Screens").progress.is_terminal());
    assert_eq!(
        (core.summary.active_agents, core.summary.stalled_agents),
        (1, 0)
    );

    world.start("links");
    let (board, row) = world.mobile_reads();
    assert_eq!(board.moving.len(), 1);
    assert_eq!(board.moving[0].step_title.as_deref(), Some("Links"));
    assert_eq!(board.moving[0].stall, None);
    let anchor = step(&world.core(), "Links")
        .eta
        .as_ref()
        .expect("eta")
        .anchor;
    assert_eq!(board.moving[0].since, anchor.to_string());
    assert_eq!((row.moving, row.stalled), (1, 0));
}

/// Accepted with another step in progress: moving on that one, though the
/// session's current step is the accepted one.
#[test]
fn an_agent_with_an_accepted_step_and_another_in_progress_is_moving_on_the_other() {
    let world = World::new(&[("shots", "Screens"), ("links", "Links")]);
    world.start("links");
    let shots = world.start("shots");
    let delivery = world.deliver("shots", &shots);
    world.review(&delivery, Verdict::Accept);

    let (board, row) = world.mobile_reads();
    assert!(board.stalled.is_empty());
    assert_eq!(board.moving.len(), 1);
    assert_eq!(board.moving[0].step_title.as_deref(), Some("Links"));
    let core = world.core();
    let focus = core.sessions[&session()].current_node.as_ref();
    assert_eq!(focus.map(NodePath::as_str), Some("shots"));
    let anchor = step(&core, "Links").eta.as_ref().expect("eta").anchor;
    assert_eq!(board.moving[0].since, anchor.to_string());
    assert_eq!((row.moving, row.stalled), (1, 0));
}

/// Sent back and live: the step is the agent's again, moving since the
/// send-back, not since the step's start.
#[test]
fn a_sent_back_step_is_the_agents_again() {
    for steps in [
        &[("shots", "Screens"), ("links", "Links")][..],
        &[("shots", "Screens")][..],
    ] {
        let world = World::new(steps);
        let attempt = world.start("shots");
        let delivery = world.deliver("shots", &attempt);
        let (board, _) = world.mobile_reads();
        assert!(board.moving.is_empty() && board.stalled.is_empty());

        world.review(&delivery, send_back());
        let (board, row) = world.mobile_reads();
        assert!(board.stalled.is_empty(), "{:?}", board.stalled);
        assert_eq!(board.moving.len(), 1, "{steps:?}");
        assert_eq!(board.moving[0].session, SESSION);
        assert_eq!(board.moving[0].step_title.as_deref(), Some("Screens"));
        assert_eq!(board.moving[0].harness, "claude_code");
        assert_eq!(board.moving[0].stall, None);
        assert_eq!((row.moving, row.stalled), (1, 0));
        let core = world.core();
        let sent_back = sent_back_at(&core, &delivery);
        let screens = step(&core, "Screens");
        assert_eq!(screens.phase, Some(Phase::SentBack));
        assert!(sent_back > screens.eta.as_ref().expect("eta").anchor);
        assert_eq!(board.moving[0].since, sent_back.to_string());

        // Delivered again: with the owner again.
        let again = world.agent_line(
            "delivery",
            json!({"node": "shots", "attempt": attempt, "supersedes": delivery}),
            json!({
                "name": "Settings screen, again",
                "demos": [{"kind": "preview_url", "body": {"url": "http://localhost:3000/"}}],
                "criteria": [], "replies": [],
                "disclosure": {"proven": ["It renders."], "not_proven_yet": [], "faked_for_now": []},
            }),
        );
        let (board, row) = world.mobile_reads();
        assert_eq!(board.needs.len(), 1, "{again}");
        assert!(board.moving.is_empty() && board.stalled.is_empty());
        assert_eq!((row.moving, row.stalled), (0, 0));
    }
}

/// Sent back and quiet: stalled as sent back, since the send-back, whether
/// the claim is still held inside its ETA (`late` false: the review wait
/// extended it) or has stalled past it. The facts asserted on the step are
/// the ones the desktop's `isStalled` reads for the same answer. When the
/// agent picks the step up it is moving again.
#[test]
fn a_sent_back_step_whose_agent_went_quiet_is_stalled_as_sent_back() {
    for late in [false, true] {
        let world = World::new(&[("shots", "Screens"), ("links", "Links")]);
        world.ago.set(7_200);
        let attempt = world.start("shots");
        if late {
            // Delivered an hour after its ETA passed.
            world.ago.set(3_600);
        }
        let delivery = world.deliver("shots", &attempt);
        let before = agent_facts(&world);
        world.review(&delivery, send_back());

        let (board, row) = world.mobile_reads();
        assert!(board.needs.is_empty(), "{:?}", board.needs);
        assert!(board.moving.is_empty(), "{:?}", board.moving);
        assert_eq!(board.stalled.len(), 1, "late {late}");
        let stalled = &board.stalled[0];
        assert_eq!(stalled.session, SESSION);
        assert_eq!(stalled.step_title.as_deref(), Some("Screens"));
        assert_eq!(stalled.stall, Some(StallReason::SentBack));
        assert_eq!((row.moving, row.stalled), (0, 1));

        let core = world.core();
        assert_eq!(stalled.since, sent_back_at(&core, &delivery).to_string());
        let screens = step(&core, "Screens");
        assert_eq!(screens.progress, Progress::Doing);
        assert_eq!(screens.phase, Some(Phase::SentBack));
        assert_eq!(screens.liveness, Liveness::Quiet);
        let lease = screens.claim.as_ref().expect("claim").state;
        assert_eq!(lease, if late { Lease::Stalled } else { Lease::Held });
        assert!(motion::sent_back_unattended(screens));

        // The send-back is the owner's event: the session, the summary's
        // agents and the Needs-you rows other than the review are as before
        // it, and the core does not stall the session.
        let agent = &core.sessions[&session()];
        assert_eq!(agent.liveness, Liveness::Quiet);
        assert!(agent.stalled_on.is_empty());
        assert_eq!(
            (core.summary.active_agents, core.summary.stalled_agents),
            (0, 0)
        );
        assert!(before.contains(&format!("{agent:?}")), "{before}");
        let owed = world.mac.owed(&slug(), &session(), false).expect("owed");
        assert_eq!((owed.yours.len(), owed.in_review), (1, 0));

        // Picked up: a work signal now.
        world.ago.set(0);
        world.pulse("shots", &attempt);
        let (board, row) = world.mobile_reads();
        assert!(board.stalled.is_empty(), "{:?}", board.stalled);
        assert_eq!(board.moving.len(), 1, "late {late}");
        assert_eq!(board.moving[0].step_title.as_deref(), Some("Screens"));
        assert_eq!((row.moving, row.stalled), (1, 0));
    }
}

/// A live session at work on another step is moving on that one; its
/// sent-back step that it left is not a second row.
#[test]
fn a_live_agent_on_another_step_is_moving_not_stalled_on_its_sent_back_one() {
    let world = World::new(&[("shots", "Screens"), ("links", "Links")]);
    world.ago.set(7_200);
    let shots = world.start("shots");
    world.ago.set(3_600);
    let delivery = world.deliver("shots", &shots);
    world.ago.set(60);
    world.start("links");
    world.review(&delivery, send_back());

    let core = world.core();
    let screens = step(&core, "Screens");
    assert_eq!(screens.claim.as_ref().expect("claim").state, Lease::Stalled);
    assert!(
        motion::sent_back_unattended(screens),
        "live, claim not held"
    );
    let (board, row) = world.mobile_reads();
    assert!(board.stalled.is_empty(), "{:?}", board.stalled);
    assert_eq!(board.moving.len(), 1);
    assert_eq!(board.moving[0].step_title.as_deref(), Some("Links"));
    assert_eq!((row.moving, row.stalled), (1, 0));
}

/// The lists are a display rule: computing them leaves what agents are told
/// from as it is, in every state after a verdict.
#[test]
fn what_agents_read_after_a_verdict_is_what_the_core_says_without_the_rule() {
    let world = World::new(&[("shots", "Screens"), ("links", "Links")]);
    let attempt = world.start("shots");
    let delivery = world.deliver("shots", &attempt);
    world.review(&delivery, Verdict::Accept);
    let before = agent_facts(&world);
    let (board, _) = world.mobile_reads();
    assert!(board.moving.is_empty());
    assert_eq!(agent_facts(&world), before);
    // The accepted step is not owed and draws no nudge about it.
    let owed = world.mac.owed(&slug(), &session(), false).expect("owed");
    assert!(owed.yours.is_empty());
    assert_eq!(owed.in_review, 0);
}

#[test]
fn a_session_with_another_step_in_progress_is_moving_on_that_one() {
    let world = World::new(&[("shots", "Screens"), ("links", "Links")]);
    let links = world.start("links");
    let shots = world.start("shots");
    // The session's current step is the one it started last.
    world.deliver("shots", &shots);

    let (board, row) = world.mobile_reads();
    assert_eq!(board.needs.len(), 1);
    assert!(board.stalled.is_empty());
    assert_eq!(board.moving.len(), 1, "one row a session");
    assert_eq!(board.moving[0].step_title.as_deref(), Some("Links"));
    let core = world.core();
    assert_eq!(
        core.sessions[&session()]
            .current_node
            .as_ref()
            .map(|node| node.as_str()),
        Some("shots")
    );
    let started = core.features.values().find(|step| step.title == "Links");
    let anchor = started
        .and_then(|step| step.eta.as_ref())
        .expect("eta")
        .anchor;
    assert_eq!(board.moving[0].since, anchor.to_string());
    assert_eq!((row.moving, row.stalled), (1, 0));

    // Both delivered: nothing of it is moving.
    world.deliver("links", &links);
    let (board, row) = world.mobile_reads();
    assert_eq!(board.needs.len(), 2);
    assert!(board.moving.is_empty() && board.stalled.is_empty());
    assert_eq!((row.moving, row.stalled), (0, 0));
}
