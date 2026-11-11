#![forbid(unsafe_code)]

//! Contract v1 writes, with two hosts on one Vault in a temporary directory:
//! the "Mac" (a core `Service`) and a mobile client (a `Session`). Agents write raw
//! lines, as an agent host would.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;

use agendar_core::error::details;
use agendar_core::guard::{decision_revision, need_revision};
use agendar_core::model::{EventId, OptionId, ProjectSlug};
use agendar_core::service::Service;
use agendar_core::vault::{Clock, SystemClock};
use agendar_ffi::{
    Choice, FfiError, Receipt, ReceiptStep, RefusalCode, RescueAction, Session, Surface,
};
use serde_json::{Value, json};

const PROJECT: &str = "mobile";
const AGENT_HOST: &str = "h-00000000000000000000000001";
const EASY: &str = "00000000000000000000000021";
const CHOOSE: &str = "00000000000000000000000022";
const CONFIRM: &str = "00000000000000000000000023";
const ASKS: [&str; 3] = [EASY, CHOOSE, CONFIRM];
/// The first of the two concurrent starts on step `n3`.
const CONTESTED_START: &str = "00000000000000000000000031";
const OWN_LABEL: &str = "Your own answer";

struct World {
    dir: tempfile::TempDir,
    mac: Service,
    mobile: Session,
    mobile_host: String,
    seqs: RefCell<BTreeMap<String, u64>>,
    next_id: Cell<u64>,
}

fn session(number: u32) -> String {
    format!("{number:026}")
}

fn agent(number: u32) -> Value {
    json!({"harness": "generic", "session": session(number)})
}

fn card(risk: &str, confidence: &str) -> Value {
    json!({
        "already_checked": [], "domain": "arch",
        "done_when": [{"id": "bytes", "text": "Demo bytes match."}],
        "external": false, "on_timeout": {"kind": "hold"},
        "options": [
            {"consequence": "Use hooks.", "eta": {"max_s": 3600, "min_s": 1800}, "external": false,
             "icon": "link", "id": "adapters", "label": "Use harness adapters", "reversibility": risk},
            {"consequence": "Use the inbox.", "eta": {"max_s": 1800, "min_s": 900}, "external": false,
             "icon": "inbox", "id": "inbox", "label": "Use inbox on next start", "reversibility": risk}
        ],
        "question": "How should answers reach idle agents?", "reach": "local",
        "recommended": {"because": "It is simpler.", "confidence": confidence, "option": "adapters"},
        "reversibility": risk, "why_now": "Wake work is blocked."
    })
}

fn policy(risk: &str) -> Value {
    json!({
        "budget": {"blocked": false, "demo_gap_s": 0, "pending_slices": 0, "reasons": [],
                   "speculative_features": 0},
        "effective": "ask", "external": false, "heuristic_hits": [], "requested": "ask",
        "reversibility": risk
    })
}

impl World {
    /// One project: steps `n0`..`n2` each hold an open ask (easy, choose,
    /// confirm), `n6` follows `n0`, `n3` was started by two agents at once
    /// (a contested rescue row), and `n4` ⇄ `n5` is a rejected cycle.
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = dir.path().join("vault");
        let (mac, _) = Service::init(&vault, &dir.path().join("mac")).expect("init");
        let mobile = Session::open(
            vault.display().to_string(),
            dir.path().join("mobile").display().to_string(),
        )
        .expect("open");
        let world = Self {
            mobile_host: mobile.health().expect("health").host,
            mobile,
            dir,
            mac,
            seqs: RefCell::new(BTreeMap::new()),
            next_id: Cell::new(100),
        };
        let operator = "operator";
        let feature = |title: &str, depends_on: Value| json!({"demo_kind": "cli_transcript", "depends_on": depends_on, "slice": "s0", "title": title});
        world.line(
            "00000000000000000000000001",
            operator,
            "project.declared",
            json!({}),
            json!({"brief": "Fixture", "goal": "Answer from a mobile client", "title": "Mobile"}),
        );
        let steps = [
            ("n0", "Easy step", json!([])),
            ("n1", "Choose step", json!([])),
            ("n2", "Confirm step", json!([])),
            ("n3", "Contested step", json!([])),
            ("n4", "Cycle head", json!([])),
            ("n5", "Cycle tail", json!(["n4"])),
            ("n6", "After easy", json!(["n0"])),
        ];
        for (index, (node, title, depends_on)) in steps.into_iter().enumerate() {
            world.line(
                &format!("{:026}", 2 + index),
                operator,
                "plan.feature",
                json!({"node": node}),
                feature(title, depends_on),
            );
        }
        // The edge that would close n4 → n5 → n4 is rejected: a cycle rescue row.
        world.line(
            "00000000000000000000000009",
            operator,
            "plan.feature",
            json!({"node": "n4"}),
            feature("Cycle head", json!(["n5"])),
        );
        let start = |id: &str, number: u32, node: &str| {
            world.line(
                id,
                &format!("s-{}", session(number)),
                "work.progress",
                json!({"node": node}),
                json!({"agent": agent(number), "eta": {"max_s": 900, "min_s": 300}, "evidence": [], "status": "started"}),
            );
        };
        let asks = [
            (EASY, 101, "n0", "easy", "med"),
            (CHOOSE, 102, "n1", "costly", "high"),
            (CONFIRM, 103, "n2", "irreversible", "high"),
        ];
        for (index, (ask, number, node, risk, confidence)) in asks.into_iter().enumerate() {
            start(&format!("{:026}", 11 + index), number, node);
            world.line(
                ask,
                &format!("s-{}", session(number)),
                "decision.ask",
                json!({"node": node}),
                json!({"agent": agent(number), "card": card(risk, confidence), "policy": policy(risk)}),
            );
        }
        start(CONTESTED_START, 104, "n3");
        start("00000000000000000000000032", 105, "n3");
        world.mobile.spike_refresh().expect("refresh");
        world
    }

    fn vault(&self) -> PathBuf {
        self.dir.path().join("vault")
    }

    /// Append one raw line as a writer of the agent host.
    fn line(&self, id: &str, writer: &str, kind: &str, refs: Value, body: Value) {
        let seq = {
            let mut seqs = self.seqs.borrow_mut();
            let seq = seqs.entry(writer.to_owned()).or_insert(0);
            *seq += 1;
            *seq
        };
        // Fixture ids are small; the first lines are a minute old, in id order.
        let number: i64 = id.parse().expect("numeric fixture id");
        let line = json!({
            "v": 1, "id": id, "seq": seq, "writer": format!("{AGENT_HOST}/{writer}"),
            "ts": SystemClock.now().add_seconds(number.min(60) - 60).canonical(),
            "project": PROJECT, "refs": refs, "kind": kind, "body": body,
        });
        let host_dir = self.vault().join("projects").join(PROJECT).join(AGENT_HOST);
        std::fs::create_dir_all(&host_dir).expect("host dir");
        let file = host_dir.join(format!("{writer}.jsonl"));
        let mut text = std::fs::read_to_string(&file).unwrap_or_default();
        text.push_str(&serde_json::to_string(&line).expect("json"));
        text.push('\n');
        std::fs::write(&file, text).expect("write line");
    }

    fn later_id(&self) -> String {
        self.next_id.set(self.next_id.get() + 1);
        format!("{:026}", self.next_id.get())
    }

    fn slug() -> ProjectSlug {
        ProjectSlug::parse(PROJECT).expect("slug")
    }

    /// The revision of a card, as the Mac reads it now.
    fn revision(&self, ask: &str) -> String {
        let fresh = self.mac.refresh(SystemClock.now(), false).expect("refresh");
        let project = &fresh.state.projects[&Self::slug()];
        let decision = &project.decisions[&EventId::parse(ask).expect("ask id")];
        decision_revision(&Self::slug(), decision, &project.policy.revision)
            .as_str()
            .to_owned()
    }

    /// The id and revision of the rescue row whose id starts with `prefix`.
    fn rescue_row(&self, prefix: &str) -> (String, String) {
        let fresh = self.mac.refresh(SystemClock.now(), false).expect("refresh");
        let project = &fresh.state.projects[&Self::slug()];
        let need = project
            .needs_you
            .iter()
            .find(|need| need.id.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix} row in {:?}", project.needs_you));
        let revision = need_revision(
            need,
            project.decisions.get(&need.source),
            &project.policy.revision,
        );
        (need.id.clone(), revision.as_str().to_owned())
    }

    fn answer(
        &self,
        ask: &str,
        choice: Choice,
        surface: Surface,
        echo: &str,
    ) -> Result<Receipt, FfiError> {
        self.mobile.answer_checked(
            ask.to_owned(),
            choice,
            self.revision(ask),
            surface,
            echo.to_owned(),
        )
    }

    fn mac_answers(&self, ask: &str, option: &str) -> String {
        self.mac
            .answer_decision(
                &Self::slug(),
                &EventId::parse(ask).expect("ask id"),
                &OptionId::parse(option).expect("option"),
                None,
                None,
                None,
            )
            .expect("the Mac answers")
            .as_str()
            .to_owned()
    }

    fn mobile_host(&self) -> String {
        self.mobile_host.clone()
    }

    /// Every line the mobile client wrote, oldest first.
    fn mobile_lines(&self) -> Vec<Value> {
        let file = self
            .vault()
            .join("projects")
            .join(PROJECT)
            .join(self.mobile_host())
            .join("operator.jsonl");
        std::fs::read_to_string(file)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("json"))
            .collect()
    }

    /// Hide one Vault file behind an iCloud placeholder, as an evicted or not
    /// yet downloaded file looks.
    fn evict(&self, relative: &str) {
        let file = self.vault().join(relative);
        let name = file.file_name().expect("name").to_string_lossy();
        std::fs::rename(&file, file.with_file_name(format!(".{name}.icloud"))).expect("evict");
    }
}

fn adapters() -> Choice {
    Choice::Option {
        id: "adapters".to_owned(),
    }
}

fn inbox() -> Choice {
    Choice::Option {
        id: "inbox".to_owned(),
    }
}

fn own(text: &str) -> Choice {
    Choice::Own {
        text: text.to_owned(),
    }
}

const ADAPTERS: &str = "Use harness adapters";
const INBOX: &str = "Use inbox on next start";

#[track_caller]
fn refused<T: std::fmt::Debug>(result: Result<T, FfiError>, expected: RefusalCode) {
    match result {
        Err(FfiError::Refused { code, message }) => {
            assert_eq!(code, expected, "{message}");
            assert!(!message.is_empty());
        }
        other => panic!("expected the {} refusal, got {other:?}", expected.as_str()),
    }
}

#[track_caller]
fn core_error<T: std::fmt::Debug>(result: Result<T, FfiError>, expected: &str) -> String {
    match result {
        Err(FfiError::Core {
            code,
            detail,
            message,
            ..
        }) => {
            assert_eq!(code, expected, "{message}");
            // The detail is a field; the sentence no longer carries it.
            assert!(!message.ends_with(']'), "{message}");
            detail.unwrap_or_default()
        }
        other => panic!("expected {expected}, got {other:?}"),
    }
}

#[test]
fn the_guard_decides_which_surface_may_answer() {
    for surface in [Surface::App, Surface::Notification, Surface::Island] {
        let world = World::new();
        // An easy card is answered from every surface.
        let receipt = world
            .answer(EASY, adapters(), surface, ADAPTERS)
            .unwrap_or_else(|error| panic!("{surface:?} answers an easy card: {error}"));
        assert_eq!(receipt.chose_label, ADAPTERS);
        // A choose or confirm card only in the app.
        for ask in [CHOOSE, CONFIRM] {
            let result = world.answer(ask, inbox(), surface, INBOX);
            if surface == Surface::App {
                assert_eq!(result.expect("the app answers every guard").ask_id, ask);
            } else {
                refused(result, RefusalCode::SurfaceNotAllowed);
            }
        }
        let answers = world.mobile_lines().len();
        assert_eq!(answers, if surface == Surface::App { 3 } else { 1 });
    }
}

#[test]
fn every_refusal_code_and_nothing_written() {
    let world = World::new();

    // not_synced: the mobile client has not read the question.
    refused(
        world.mobile.answer_checked(
            "00000000000000000000009999".to_owned(),
            adapters(),
            "0".repeat(64),
            Surface::App,
            ADAPTERS.to_owned(),
        ),
        RefusalCode::NotSynced,
    );

    // stale_revision: the question is open, but not as the caller read it.
    refused(
        world.mobile.answer_checked(
            EASY.to_owned(),
            adapters(),
            "0".repeat(64),
            Surface::App,
            ADAPTERS.to_owned(),
        ),
        RefusalCode::StaleRevision,
    );

    // surface_not_allowed.
    refused(
        world.answer(CONFIRM, adapters(), Surface::Island, ADAPTERS),
        RefusalCode::SurfaceNotAllowed,
    );

    // withdrawn: the agent took the question back; the revision the caller
    // holds is the one from before.
    let before = world.revision(CHOOSE);
    world.line(
        &world.later_id(),
        &format!("s-{}", session(102)),
        "decision.withdrawn",
        json!({"node": "n1", "ask": CHOOSE}),
        json!({"agent": agent(102), "reason": "No longer needed."}),
    );
    refused(
        world.mobile.answer_checked(
            CHOOSE.to_owned(),
            adapters(),
            before,
            Surface::App,
            ADAPTERS.to_owned(),
        ),
        RefusalCode::Withdrawn,
    );

    // already_answered: the Mac answered first.
    let before = world.revision(EASY);
    world.mac_answers(EASY, "inbox");
    refused(
        world.mobile.answer_checked(
            EASY.to_owned(),
            adapters(),
            before,
            Surface::App,
            ADAPTERS.to_owned(),
        ),
        RefusalCode::AlreadyAnswered,
    );

    // not_synced: a file of the project is still to download.
    world.evict(&format!(
        "projects/{PROJECT}/{AGENT_HOST}/s-{}.jsonl",
        session(104)
    ));
    refused(
        world.answer(CONFIRM, adapters(), Surface::App, ADAPTERS),
        RefusalCode::NotSynced,
    );

    assert!(world.mobile_lines().is_empty(), "a refusal writes nothing");
}

#[test]
fn a_refresh_that_cannot_finish_in_time_is_refused() {
    let world = World::new();
    let revision = world.revision(EASY);
    // Another reader of the mobile client's index holds the ingest lock, and the
    // Vault has a line the mobile client has not read: the refresh must wait.
    let mobile = Service::open(&world.vault(), &world.dir.path().join("mobile")).expect("open");
    let lock = mobile
        .local()
        .locks_dir(mobile.vault().id())
        .join("ingest.lock");
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock)
        .expect("lock file");
    held.lock().expect("hold the ingest lock");
    world.line(
        &world.later_id(),
        &format!("s-{}", session(101)),
        "pulse",
        json!({"node": "n0", "attempt": "00000000000000000000000011"}),
        json!({"agent": agent(101), "message": "Working.", "boundary": false}),
    );
    let started = std::time::Instant::now();
    refused(
        world.mobile.answer_checked(
            EASY.to_owned(),
            adapters(),
            revision.clone(),
            Surface::App,
            ADAPTERS.to_owned(),
        ),
        RefusalCode::RefreshTimeout,
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    assert!(world.mobile_lines().is_empty());

    held.unlock().expect("release");
    world
        .mobile
        .answer_checked(
            EASY.to_owned(),
            adapters(),
            revision,
            Surface::App,
            ADAPTERS.to_owned(),
        )
        .expect("the same request passes once the index is free");
}

#[test]
fn the_echoed_label_is_checked_on_every_guard() {
    let world = World::new();
    for ask in ASKS {
        // The label of another option, and no label at all.
        let detail = core_error(
            world.answer(ask, adapters(), Surface::App, INBOX),
            "E_VALIDATION",
        );
        assert_eq!(detail, "echo_mismatch");
        core_error(
            world.answer(ask, adapters(), Surface::App, ""),
            "E_VALIDATION",
        );
    }
    // An option the card does not have.
    let detail = core_error(
        world.answer(
            EASY,
            Choice::Option {
                id: "nope".to_owned(),
            },
            Surface::App,
            ADAPTERS,
        ),
        "E_VALIDATION",
    );
    assert_eq!(detail, "unknown_option");
    assert!(world.mobile_lines().is_empty());

    // The label as shown passes; surrounding spaces do not matter.
    for ask in ASKS {
        world
            .answer(ask, adapters(), Surface::App, &format!(" {ADAPTERS} "))
            .expect("the echoed label matches");
    }
}

#[test]
fn own_words_have_limits_and_are_written_in_the_app_only() {
    let world = World::new();
    for surface in [Surface::Notification, Surface::Island] {
        refused(
            world.answer(EASY, own("Do both."), surface, OWN_LABEL),
            RefusalCode::SurfaceNotAllowed,
        );
    }
    let detail = core_error(
        world.answer(EASY, own("  \n "), Surface::App, OWN_LABEL),
        "E_VALIDATION",
    );
    assert_eq!(detail, "own_answer_missing");
    let detail = core_error(
        world.answer(EASY, own(&"é".repeat(601)), Surface::App, OWN_LABEL),
        "E_VALIDATION",
    );
    assert_eq!(detail, "own_answer_too_long");
    // The echo of an own answer is the reserved option's label.
    core_error(
        world.answer(CHOOSE, own("Do both."), Surface::App, "Do both."),
        "E_VALIDATION",
    );
    assert!(world.mobile_lines().is_empty());

    // 600 characters (not bytes) pass, trimmed.
    let longest = "é".repeat(600);
    let receipt = world
        .answer(
            EASY,
            own(&format!("  {longest}\n")),
            Surface::App,
            OWN_LABEL,
        )
        .expect("600 characters");
    assert_eq!(receipt.chose_label, OWN_LABEL);
    assert_eq!(receipt.own_text.as_deref(), Some(longest.as_str()));
    let receipt = world
        .answer(CONFIRM, own("Do both."), Surface::App, OWN_LABEL)
        .expect("own words on a confirm card");
    assert_eq!(receipt.own_text.as_deref(), Some("Do both."));

    let lines = world.mobile_lines();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["kind"], "decision.answer");
    assert_eq!(lines[1]["body"]["option"], "own");
    assert_eq!(lines[1]["body"]["own_answer"], "Do both.");
}

/// Every `E_VALIDATION` a mobile client's own input can cause on the writes and
/// the reads beside them: the detail names the rule and the field the input.
#[test]
fn every_invalid_request_names_its_rule_and_its_input() {
    let world = World::new();
    let revision = world.revision(EASY);
    let answer = |ask: &str, choice: Choice, revision: &str, echo: &str| {
        world
            .mobile
            .answer_checked(
                ask.to_owned(),
                choice,
                revision.to_owned(),
                Surface::App,
                echo.to_owned(),
            )
            .map(|_| ())
    };
    let rescue = |project: &str, need: &str, revision: &str, action| {
        world
            .mobile
            .rescue(
                project.to_owned(),
                need.to_owned(),
                revision.to_owned(),
                action,
            )
            .map(|_| ())
    };
    let (cycle, cycle_revision) = world.rescue_row("rescue:cycle:");
    let malformed = Choice::Option {
        id: "Not An Id".to_owned(),
    };
    let unknown = Choice::Option {
        id: "nope".to_owned(),
    };
    let mobile = &world.mobile;
    let rows: Vec<(&str, Result<(), FfiError>, &str, &str)> = vec![
        (
            "answer: the ask id",
            answer("not an id", adapters(), &revision, ADAPTERS),
            "invalid_event_id",
            "/ask_id",
        ),
        (
            "answer: the revision",
            answer(EASY, adapters(), "not a revision", ADAPTERS),
            "invalid_revision",
            "/expected_revision",
        ),
        (
            "answer: the option id",
            answer(EASY, malformed, &revision, ADAPTERS),
            "invalid_option_id",
            "/choice",
        ),
        (
            "answer: an option the card does not have",
            answer(EASY, unknown, &revision, ADAPTERS),
            "unknown_option",
            "/choice",
        ),
        (
            "answer: own words left blank",
            answer(EASY, own(" \n "), &revision, OWN_LABEL),
            "own_answer_missing",
            "/choice",
        ),
        (
            "answer: own words too long",
            answer(EASY, own(&"é".repeat(601)), &revision, OWN_LABEL),
            "own_answer_too_long",
            "/choice",
        ),
        (
            "answer: own words with a NUL",
            answer(EASY, own("Do\u{0}both."), &revision, OWN_LABEL),
            "control_character",
            "/choice",
        ),
        (
            "answer: another option's label",
            answer(EASY, adapters(), &revision, INBOX),
            "echo_mismatch",
            "/echo_label",
        ),
        (
            "answer: no label on a card that needs one",
            answer(CHOOSE, adapters(), &world.revision(CHOOSE), ""),
            "echo_mismatch",
            "/echo_label",
        ),
        (
            "rescue: the project",
            rescue("Not A Slug", &cycle, &cycle_revision, RescueAction::Pause),
            "invalid_project",
            "/project",
        ),
        (
            "rescue: the revision",
            rescue(PROJECT, &cycle, "not a revision", RescueAction::Pause),
            "invalid_revision",
            "/expected_revision",
        ),
        (
            "rescue: a row that is a question",
            rescue(
                PROJECT,
                &format!("decide:{EASY}"),
                &revision,
                RescueAction::Pause,
            ),
            "not_a_rescue",
            "/need_id",
        ),
        (
            "rescue: nobody to hand the step to",
            rescue(PROJECT, &cycle, &cycle_revision, RescueAction::Handoff),
            "handoff_not_offered",
            "/action",
        ),
        (
            "receipt: the answer id",
            mobile.receipt("not an id".to_owned()).map(|_| ()),
            "invalid_event_id",
            "/answer_id",
        ),
        (
            "card: the ask id",
            mobile.card("not an id".to_owned()).map(|_| ()),
            "invalid_event_id",
            "/ask_id",
        ),
        (
            "delivery: the delivery id",
            mobile.delivery("not an id".to_owned()).map(|_| ()),
            "invalid_event_id",
            "/delivery_id",
        ),
        (
            "project: the slug",
            mobile.project("Not A Slug".to_owned()).map(|_| ()),
            "invalid_project",
            "/slug",
        ),
        (
            "now: when the owner last looked",
            mobile.now(Some("yesterday".to_owned())).map(|_| ()),
            "invalid_time",
            "/looked_at",
        ),
    ];
    for (name, result, want_detail, want_field) in rows {
        match result {
            Err(FfiError::Core {
                code,
                detail,
                field,
                retryable,
                message,
            }) => {
                assert_eq!(code, "E_VALIDATION", "{name}: {message}");
                assert_eq!(detail.as_deref(), Some(want_detail), "{name}: {message}");
                assert_eq!(field.as_deref(), Some(want_field), "{name}: {message}");
                assert!(!retryable, "{name}");
                assert_eq!(details::listed(want_detail), Some(want_detail), "{name}");
                assert!(!message.ends_with(']'), "{name}: {message}");
            }
            other => panic!("{name}: expected E_VALIDATION ({want_detail}), got {other:?}"),
        }
    }
    assert!(world.mobile_lines().is_empty());

    // What is not there is `E_NOT_FOUND` with its own detail, not bad input.
    for (result, want) in [
        (
            mobile
                .receipt("00000000000000000000009999".to_owned())
                .map(|_| ()),
            "answer_not_found",
        ),
        (
            mobile
                .card("00000000000000000000009999".to_owned())
                .map(|_| ()),
            "not_found",
        ),
        (
            mobile.project("nowhere".to_owned()).map(|_| ()),
            "project_not_found",
        ),
    ] {
        let Err(FfiError::Core { code, detail, .. }) = result else {
            panic!("expected E_NOT_FOUND ({want})");
        };
        assert_eq!(
            (code.as_str(), detail.as_deref()),
            ("E_NOT_FOUND", Some(want))
        );
    }
}

#[test]
fn a_repeated_call_gives_one_answer() {
    let world = World::new();
    let revision = world.revision(CHOOSE);
    let call = || {
        world.mobile.answer_checked(
            CHOOSE.to_owned(),
            inbox(),
            revision.clone(),
            Surface::App,
            INBOX.to_owned(),
        )
    };
    let first = call().expect("answer");
    // The same arguments again, with the revision from before the answer.
    let second = call().expect("the repeat is the same answer");
    assert_eq!(second, first);
    let third = call().expect("and again");
    assert_eq!(third.answer_id, first.answer_id);

    let lines = world.mobile_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["id"], first.answer_id.as_str());
    assert_eq!(lines[0]["refs"]["ask"], CHOOSE);
    assert_eq!(lines[0]["body"]["option"], "inbox");

    // A different choice for the answered question is not a repeat.
    refused(
        world.answer(CHOOSE, adapters(), Surface::App, ADAPTERS),
        RefusalCode::AlreadyAnswered,
    );
    // Own words repeat the same way.
    let own_first = world
        .answer(EASY, own("Do both."), Surface::App, OWN_LABEL)
        .expect("own");
    let own_again = world
        .answer(EASY, own(" Do both. "), Surface::App, OWN_LABEL)
        .expect("repeat");
    assert_eq!(own_again.answer_id, own_first.answer_id);
    refused(
        world.answer(EASY, own("Something else."), Surface::App, OWN_LABEL),
        RefusalCode::AlreadyAnswered,
    );
    assert_eq!(world.mobile_lines().len(), 2);
}

#[test]
fn the_mac_answers_first() {
    let world = World::new();
    let before = world.revision(CONFIRM);
    let mac_answer = world.mac_answers(CONFIRM, "adapters");
    // The mobile client still shows the open card; its answer is refused, even the
    // same choice, because the answer in effect is not the mobile client's.
    for choice in [inbox(), adapters()] {
        let label = if choice == inbox() { INBOX } else { ADAPTERS };
        refused(
            world.mobile.answer_checked(
                CONFIRM.to_owned(),
                choice,
                before.clone(),
                Surface::App,
                label.to_owned(),
            ),
            RefusalCode::AlreadyAnswered,
        );
    }
    assert!(world.mobile_lines().is_empty());
    // The Mac's answer has a receipt on the mobile client too.
    let receipt = world.mobile.receipt(mac_answer.clone()).expect("receipt");
    assert_eq!(receipt.answer_id, mac_answer);
    assert_eq!(receipt.ask_id, CONFIRM);
    assert_eq!(receipt.chose_label, ADAPTERS);
    assert!(!receipt.writer_file.contains(&world.mobile_host()));
    assert!(world.vault().join(&receipt.writer_file).is_file());
}

#[test]
fn the_receipt_before_and_after_the_agent_has_the_answer() {
    let world = World::new();
    let saved = world
        .answer(EASY, adapters(), Surface::Notification, ADAPTERS)
        .expect("answer");
    assert_eq!(saved.ask_id, EASY);
    assert_eq!(saved.project, PROJECT);
    assert_eq!(saved.chose_label, ADAPTERS);
    assert_eq!(saved.own_text, None);
    assert_eq!(saved.step, ReceiptStep::Saved);
    assert_eq!(saved.acked_at, None);
    assert!(saved.released.is_empty());
    assert_eq!(
        saved.writer_file,
        format!("projects/{PROJECT}/{}/operator.jsonl", world.mobile_host())
    );
    let written = std::fs::read_to_string(world.vault().join(&saved.writer_file)).expect("file");
    assert!(written.contains(&saved.answer_id));
    // The next thing that needs the owner is another row, not this question.
    let next = saved.next_need_id.clone().expect("more needs");
    assert!(!next.contains(EASY), "{next}");
    assert_eq!(
        world.mobile.receipt(saved.answer_id.clone()),
        Ok(saved.clone())
    );

    // The agent acknowledges; the mobile client sees it at its next refresh.
    world.line(
        &world.later_id(),
        &format!("s-{}", session(101)),
        "decision.ack",
        json!({"node": "n0", "ask": EASY, "answer": saved.answer_id}),
        json!({"agent": agent(101), "applied_option": "adapters", "restarted": []}),
    );
    assert_eq!(
        world
            .mobile
            .receipt(saved.answer_id.clone())
            .expect("cached")
            .step,
        ReceiptStep::Saved
    );
    world.mobile.spike_refresh().expect("refresh");
    let acked = world
        .mobile
        .receipt(saved.answer_id.clone())
        .expect("receipt");
    assert_eq!(acked.step, ReceiptStep::AgentHasIt);
    assert!(acked.acked_at.is_some());
    assert_eq!(acked.released, ["Easy step", "After easy"]);
    assert_eq!(acked.answer_id, saved.answer_id);
    assert_eq!(acked.written_at, saved.written_at);
    assert_eq!(acked.next_need_id, saved.next_need_id);

    core_error(
        world
            .mobile
            .receipt("00000000000000000000009999".to_owned()),
        "E_NOT_FOUND",
    );
    core_error(world.mobile.receipt("not an id".to_owned()), "E_VALIDATION");
}

#[test]
fn a_rescue_is_paused_once_and_a_stale_one_is_refused() {
    let world = World::new();
    let (need, revision) = world.rescue_row("rescue:start-conflict:");
    assert!(need.ends_with(CONTESTED_START), "{need}");
    let rescue = |need: &str, revision: &str, action| {
        world.mobile.rescue(
            PROJECT.to_owned(),
            need.to_owned(),
            revision.to_owned(),
            action,
        )
    };

    // A revision the row does not have, a row that does not exist, a row that
    // is a question, a project that does not exist.
    refused(
        rescue(&need, &"0".repeat(64), RescueAction::Pause),
        RefusalCode::StaleRevision,
    );
    refused(
        rescue(
            "rescue:start-conflict:00000000000000000000009999",
            &revision,
            RescueAction::Pause,
        ),
        RefusalCode::StaleRevision,
    );
    let detail = core_error(
        rescue(
            &format!("decide:{EASY}"),
            &world.revision(EASY),
            RescueAction::Pause,
        ),
        "E_VALIDATION",
    );
    assert_eq!(detail, "not_a_rescue");
    core_error(
        world.mobile.rescue(
            "nowhere".to_owned(),
            need.clone(),
            revision.clone(),
            RescueAction::Pause,
        ),
        "E_NOT_FOUND",
    );
    assert!(world.mobile_lines().is_empty());

    let paused = rescue(&need, &revision, RescueAction::Pause).expect("pause");
    let again = rescue(&need, &revision, RescueAction::Pause).expect("the repeat");
    assert_eq!(again, paused);
    let lines = world.mobile_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["id"], paused.as_str());
    assert_eq!(lines[0]["kind"], "plan.feature");
    assert_eq!(lines[0]["refs"]["node"], "n3");
    assert_eq!(lines[0]["refs"]["causes"], json!([CONTESTED_START]));
    assert_eq!(lines[0]["body"]["paused"], true);
    // The Mac reads the step as paused.
    let fresh = world
        .mac
        .refresh(SystemClock.now(), false)
        .expect("refresh");
    let project = &fresh.state.projects[&World::slug()];
    assert!(
        project
            .features
            .values()
            .any(|step| step.title == "Contested step" && step.paused)
    );
}

#[test]
fn a_handoff_only_settles_a_contested_step() {
    let world = World::new();
    let rescue = |need: &str, revision: &str, action| {
        world.mobile.rescue(
            PROJECT.to_owned(),
            need.to_owned(),
            revision.to_owned(),
            action,
        )
    };
    // The cycle row is not a question of who continues.
    let (cycle, cycle_revision) = world.rescue_row("rescue:cycle:");
    let detail = core_error(
        rescue(&cycle, &cycle_revision, RescueAction::Handoff),
        "E_VALIDATION",
    );
    assert_eq!(detail, "handoff_not_offered");
    assert!(world.mobile_lines().is_empty());

    let (need, revision) = world.rescue_row("rescue:start-conflict:");
    let handed = rescue(&need, &revision, RescueAction::Handoff).expect("handoff");
    let lines = world.mobile_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["id"], handed.as_str());
    assert_eq!(lines[0]["body"]["handoff"], true);
    assert_eq!(lines[0]["refs"]["node"], "n3");
    let again = rescue(&need, &revision, RescueAction::Handoff).expect("the repeat");
    assert_eq!(again, handed);
    assert_eq!(world.mobile_lines().len(), 1);

    // A rescue waits for the project's files like an answer does.
    world.evict(&format!(
        "projects/{PROJECT}/{AGENT_HOST}/s-{}.jsonl",
        session(103)
    ));
    refused(
        rescue(&cycle, &cycle_revision, RescueAction::Pause),
        RefusalCode::NotSynced,
    );
    assert_eq!(world.mobile_lines().len(), 1);
}

const ASSUMED: &str = "00000000000000000000000043";

/// One more step with an ask in assume mode, on which the agent went ahead
/// with the recommended `adapters`: the ask stays open with an assumed receipt.
fn agent_goes_ahead(world: &World) {
    let writer = format!("s-{}", session(106));
    let mut assume = policy("easy");
    assume["requested"] = json!("assume");
    assume["effective"] = json!("assume");
    world.line(
        "00000000000000000000000041",
        "operator",
        "plan.feature",
        json!({"node": "n7"}),
        json!({"demo_kind": "cli_transcript", "depends_on": [], "slice": "s0", "title": "Assumed step"}),
    );
    world.line(
        "00000000000000000000000042",
        &writer,
        "work.progress",
        json!({"node": "n7"}),
        json!({"agent": agent(106), "eta": {"max_s": 900, "min_s": 300}, "evidence": [], "status": "started"}),
    );
    world.line(
        ASSUMED,
        &writer,
        "decision.ask",
        json!({"node": "n7"}),
        json!({"agent": agent(106), "card": card("easy", "med"), "policy": assume}),
    );
    world.line(
        "00000000000000000000000044",
        &writer,
        "decision.assumed",
        json!({"node": "n7", "ask": ASSUMED, "attempt": "00000000000000000000000042"}),
        json!({"agent": agent(106), "option": "adapters", "reason": "policy", "policy": assume}),
    );
}

fn assumed_lifecycle(world: &World) -> (String, String, bool) {
    let fresh = world
        .mac
        .refresh(SystemClock.now(), false)
        .expect("refresh");
    let decision =
        &fresh.state.projects[&World::slug()].decisions[&EventId::parse(ASSUMED).expect("ask id")];
    (
        decision.lifecycle.as_str().to_owned(),
        format!("{:?}", decision.mode).to_lowercase(),
        decision.effective_receipt.is_some(),
    )
}

#[test]
fn an_ask_the_agent_went_ahead_on_is_still_answered() {
    // Confirming the agent's choice, and overriding it.
    for (choice, label) in [(adapters(), ADAPTERS), (inbox(), INBOX)] {
        let world = World::new();
        agent_goes_ahead(&world);
        assert_eq!(
            assumed_lifecycle(&world),
            ("open".to_owned(), "assume".to_owned(), true)
        );

        let receipt = world
            .answer(ASSUMED, choice.clone(), Surface::App, label)
            .expect("an assumed ask is answered");
        assert_eq!(receipt.chose_label, label);
        let lines = world.mobile_lines();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0]["id"], receipt.answer_id.as_str());
        assert_ne!(assumed_lifecycle(&world).0, "open");
        // The repeat is still the one answer.
        let again = world
            .answer(ASSUMED, choice, Surface::App, label)
            .expect("repeat");
        assert_eq!(again.answer_id, receipt.answer_id);
        assert_eq!(world.mobile_lines().len(), 1);
    }
}
