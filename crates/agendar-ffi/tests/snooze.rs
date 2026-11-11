#![forbid(unsafe_code)]

//! Contract v1 `snooze`, with two hosts on one Vault in a temporary
//! directory: the "Mac" (a core `Service`) and a mobile client (a `Session`). A
//! snooze on either hides the need on both; a refusal is typed and writes
//! nothing.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

use agendar_core::model::{ProjectSlug, UtcTime};
use agendar_core::service::Service;
use agendar_core::vault::{Clock, SystemClock};
use agendar_ffi::{FfiError, NowBoard, RefusalCode, Session};
use serde_json::{Value, json};

const PROJECT: &str = "mobile";
const AGENT_HOST: &str = "h-00000000000000000000000001";
const ASK: &str = "00000000000000000000000021";
const OTHER_ASK: &str = "00000000000000000000000022";
const HOUR: i64 = 60 * 60;
const DAY: i64 = 24 * HOUR;

struct World {
    dir: tempfile::TempDir,
    mac: Service,
    mobile: Session,
    mobile_host: String,
    seqs: RefCell<BTreeMap<String, u64>>,
}

fn session(number: u32) -> String {
    format!("{number:026}")
}

fn agent(number: u32) -> Value {
    json!({"harness": "generic", "session": session(number)})
}

fn card(question: &str) -> Value {
    json!({
        "already_checked": [], "domain": "arch",
        "done_when": [{"id": "bytes", "text": "Demo bytes match."}],
        "external": false, "on_timeout": {"kind": "hold"},
        "options": [
            {"consequence": "Use hooks.", "external": false, "icon": "link", "id": "adapters",
             "label": "Use harness adapters", "reversibility": "costly"},
            {"consequence": "Use the inbox.", "external": false, "icon": "inbox", "id": "inbox",
             "label": "Use inbox on next start", "reversibility": "costly"}
        ],
        "question": question, "reach": "local",
        "recommended": {"because": "It is simpler.", "confidence": "high", "option": "adapters"},
        "reversibility": "costly", "why_now": "Wake work is blocked."
    })
}

fn policy() -> Value {
    json!({
        "budget": {"blocked": false, "demo_gap_s": 0, "pending_slices": 0, "reasons": [],
                   "speculative_features": 0},
        "effective": "ask", "external": false, "heuristic_hits": [], "requested": "ask",
        "reversibility": "costly"
    })
}

fn need(ask: &str) -> String {
    format!("decide:{ask}")
}

impl World {
    /// One project with two steps, each holding an open question.
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
        };
        world.line(
            "00000000000000000000000001",
            "operator",
            "project.declared",
            json!({}),
            json!({"brief": "Fixture", "goal": "Snooze from a mobile client", "title": "Mobile"}),
        );
        for (index, (node, ask, question)) in [
            ("n0", ASK, "How should answers reach idle agents?"),
            ("n1", OTHER_ASK, "Which store keeps the receipts?"),
        ]
        .into_iter()
        .enumerate()
        {
            let number = 101 + index as u32;
            let writer = format!("s-{}", session(number));
            world.line(
                &format!("{:026}", 2 + index),
                "operator",
                "plan.feature",
                json!({"node": node}),
                json!({"demo_kind": "cli_transcript", "depends_on": [], "slice": "s0", "title": node}),
            );
            world.line(
                &format!("{:026}", 11 + index),
                &writer,
                "work.progress",
                json!({"node": node}),
                json!({"agent": agent(number), "eta": {"max_s": 900, "min_s": 300},
                       "evidence": [], "status": "started"}),
            );
            world.line(
                ask,
                &writer,
                "decision.ask",
                json!({"node": node}),
                json!({"agent": agent(number), "card": card(question), "policy": policy()}),
            );
        }
        world.mobile.refresh(5_000).expect("refresh");
        world
    }

    fn vault(&self) -> PathBuf {
        self.dir.path().join("vault")
    }

    /// Append one raw line as a writer of the agent host, a minute old.
    fn line(&self, id: &str, writer: &str, kind: &str, refs: Value, body: Value) {
        let seq = {
            let mut seqs = self.seqs.borrow_mut();
            let seq = seqs.entry(writer.to_owned()).or_insert(0);
            *seq += 1;
            *seq
        };
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

    /// Every line the mobile client wrote, oldest first.
    fn mobile_lines(&self) -> Vec<Value> {
        let file = self
            .vault()
            .join("projects")
            .join(PROJECT)
            .join(&self.mobile_host)
            .join("operator.jsonl");
        std::fs::read_to_string(file)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("json"))
            .collect()
    }

    /// The mobile client's Now screen after it has read the Vault.
    fn board(&self) -> NowBoard {
        self.mobile.refresh(5_000).expect("refresh");
        self.mobile.now(None).expect("now")
    }

    /// When the Mac shows the need as snoozed until, if it does.
    fn mac_sees(&self, need_id: &str) -> Option<UtcTime> {
        let fresh = self.mac.refresh(SystemClock.now(), false).expect("refresh");
        fresh.state.projects[&ProjectSlug::parse(PROJECT).expect("slug")]
            .needs_you
            .iter()
            .find(|need| need.id == need_id)
            .expect("the Mac lists the need")
            .snoozed_until
    }

    fn mac_snoozes(&self, need_id: &str, until: UtcTime) -> String {
        let fresh = self.mac.refresh(SystemClock.now(), false).expect("refresh");
        self.mac
            .snooze_need(&fresh, need_id, until)
            .expect("the Mac snoozes")
            .event
            .as_str()
            .to_owned()
    }
}

impl World {
    /// One more step with an open question whose default goes ahead
    /// `after_s` after it was asked. `number` names the step and its ids.
    fn ask_with_default(&self, number: u32, after_s: i64) -> String {
        let node = format!("d{number}");
        let writer = format!("s-{}", session(number));
        let ask = format!("{:026}", number + 2);
        let mut card = card("Which default is right?");
        card["on_timeout"] = json!({"kind": "proceed_as_assumption", "after_s": after_s});
        self.line(
            &format!("{number:026}"),
            "operator",
            "plan.feature",
            json!({"node": node}),
            json!({"demo_kind": "cli_transcript", "depends_on": [], "slice": "s0", "title": node}),
        );
        self.line(
            &format!("{:026}", number + 1),
            &writer,
            "work.progress",
            json!({"node": node}),
            json!({"agent": agent(number), "eta": {"max_s": 900, "min_s": 300},
                   "evidence": [], "status": "started"}),
        );
        self.line(
            &ask,
            &writer,
            "decision.ask",
            json!({"node": node}),
            json!({"agent": agent(number), "card": card, "policy": policy()}),
        );
        need(&ask)
    }
}

fn ids(rows: &[agendar_ffi::NeedRow]) -> Vec<&str> {
    rows.iter().map(|row| row.id.as_str()).collect()
}

fn in_seconds(seconds: i64) -> UtcTime {
    SystemClock.now().add_seconds(seconds)
}

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
fn invalid<T: std::fmt::Debug>(result: Result<T, FfiError>, expected: &str) {
    match result {
        Err(FfiError::Core {
            code,
            detail,
            field,
            retryable,
            message,
        }) => {
            assert_eq!(code, "E_VALIDATION", "{message}");
            assert_eq!(detail.as_deref(), Some(expected), "{message}");
            assert_eq!(field.as_deref(), Some("/until"), "{message}");
            assert!(!retryable && !message.contains(expected), "{message}");
        }
        other => panic!("expected E_VALIDATION ({expected}), got {other:?}"),
    }
}

#[test]
fn a_snooze_on_the_mobile_client_hides_the_need_on_the_mac_and_the_mac_can_end_it() {
    let world = World::new();
    let before = world.board();
    assert_eq!(ids(&before.needs).len(), 2);
    assert!(before.snoozed.is_empty());
    let order: Vec<String> = before.needs.iter().map(|row| row.id.clone()).collect();

    let until = in_seconds(HOUR);
    let event = world
        .mobile
        .snooze(need(ASK), until.canonical())
        .expect("the mobile client snoozes");

    // The mobile client: the need left `needs` and is listed under `snoozed`.
    let board = world.board();
    assert_eq!(ids(&board.needs), [need(OTHER_ASK)]);
    assert_eq!(ids(&board.snoozed), [need(ASK)]);
    assert_eq!(
        board.snoozed[0].snoozed_until.as_deref(),
        Some(until.canonical().as_str())
    );
    assert_eq!(board.needs[0].snoozed_until, None);
    // The project page still lists it, marked.
    let page = world.mobile.project(PROJECT.to_owned()).expect("page");
    let marked: Vec<&str> = page
        .needs
        .iter()
        .filter(|row| row.snoozed_until.is_some())
        .map(|row| row.id.as_str())
        .collect();
    assert_eq!(marked, [need(ASK)]);
    assert_eq!(page.needs.len(), 2);

    // The mobile client wrote one owner event, and the Mac reads it.
    let lines = world.mobile_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["id"], json!(event));
    assert_eq!(lines[0]["kind"], "need.snoozed");
    assert_eq!(lines[0]["body"]["need"], json!(need(ASK)));
    assert_eq!(lines[0]["body"]["until"], json!(until));
    assert_eq!(
        lines[0]["body"]["revision"],
        json!(
            before
                .needs
                .iter()
                .find(|row| row.id == need(ASK))
                .expect("row")
                .revision
        )
    );
    assert_eq!(world.mac_sees(&need(ASK)), Some(until));
    assert_eq!(world.mac_sees(&need(OTHER_ASK)), None);

    // The Mac ends it with a time that has passed: back on the mobile client,
    // in the order it had.
    world.mac_snoozes(&need(ASK), in_seconds(-1));
    let board = world.board();
    assert!(board.snoozed.is_empty());
    let back: Vec<String> = board.needs.iter().map(|row| row.id.clone()).collect();
    assert_eq!(back, order);

    // The Mac snoozes the other one; the mobile client ends that with a past time.
    let mac_until = in_seconds(2 * HOUR);
    world.mac_snoozes(&need(OTHER_ASK), mac_until);
    let board = world.board();
    assert_eq!(ids(&board.snoozed), [need(OTHER_ASK)]);
    world
        .mobile
        .snooze(need(OTHER_ASK), "2020-01-01T00:00:00Z".to_owned())
        .expect("a time in the past ends the snooze");
    assert!(world.board().snoozed.is_empty());
    assert_eq!(world.mac_sees(&need(OTHER_ASK)), None);
}

#[test]
fn a_repeat_writes_nothing_and_the_latest_snooze_holds() {
    let world = World::new();
    let until = in_seconds(DAY).canonical();
    let first = world
        .mobile
        .snooze(need(ASK), until.clone())
        .expect("snooze");
    let again = world
        .mobile
        .snooze(need(ASK), until)
        .expect("the same again");
    assert_eq!(again, first);
    assert_eq!(world.mobile_lines().len(), 1);

    // Both devices snooze the same need; the later one is what both show.
    let mac_until = in_seconds(3 * HOUR);
    world.mac_snoozes(&need(ASK), mac_until);
    let board = world.board();
    assert_eq!(
        board.snoozed[0].snoozed_until.as_deref(),
        Some(mac_until.canonical().as_str())
    );
    assert_eq!(world.mac_sees(&need(ASK)), Some(mac_until));
}

#[test]
fn a_changed_need_is_shown_again() {
    let world = World::new();
    world
        .mobile
        .snooze(need(ASK), in_seconds(DAY).canonical())
        .expect("snooze");
    assert_eq!(ids(&world.board().snoozed), [need(ASK)]);
    // The owner changes the decision policy at the Mac: the card's revision
    // moves, so the snooze of the old card no longer hides it.
    let policy = serde_json::from_value(json!({
        "preset": "strict",
        "cells": {"easy_local": "ask", "easy_wide": "ask", "costly_local": "ask",
                  "costly_wide": "ask", "irreversible_local": "ask", "irreversible_wide": "ask"},
        "domain_floor": {},
        "budget": {"max_speculative_features": 10, "max_pending_slices": 2, "max_demo_gap_s": 14400},
        "require_human_review": true, "allow_timeouts": false,
    }))
    .expect("policy");
    let slug = ProjectSlug::parse(PROJECT).expect("slug");
    let current = world
        .mac
        .refresh(SystemClock.now(), false)
        .expect("refresh")
        .state
        .projects[&slug]
        .policy
        .revision
        .clone();
    world
        .mac
        .set_policy(&slug, policy, &current)
        .expect("set policy");
    let board = world.board();
    assert!(board.snoozed.is_empty());
    assert_eq!(board.needs.len(), 2);
}

#[test]
fn every_refusal_is_typed_and_writes_nothing() {
    let world = World::new();
    let hour = || in_seconds(HOUR).canonical();

    // Not a time, and more than seven days ahead.
    for not_a_time in [
        "tomorrow",
        "",
        "2026-13-01T00:00:00Z",
        "2026-01-01T00:00:00+02:00",
    ] {
        invalid(
            world.mobile.snooze(need(ASK), not_a_time.to_owned()),
            "invalid_time",
        );
    }
    invalid(
        world
            .mobile
            .snooze(need(ASK), in_seconds(7 * DAY + 60).canonical()),
        "snooze_too_long",
    );
    // A need that is not waiting, to snooze or to end.
    for until in [hour(), in_seconds(-60).canonical()] {
        refused(
            world.mobile.snooze("decide:not-waiting".to_owned(), until),
            RefusalCode::StaleRevision,
        );
    }
    // The question was withdrawn: its need is gone.
    world.line(
        "00000000000000000000000041",
        &format!("s-{}", session(101)),
        "decision.withdrawn",
        json!({"node": "n0", "ask": ASK}),
        json!({"agent": agent(101), "reason": "No longer needed."}),
    );
    refused(
        world.mobile.snooze(need(ASK), hour()),
        RefusalCode::StaleRevision,
    );

    // not_synced: a file of the project is still to download.
    let file = world
        .vault()
        .join("projects")
        .join(PROJECT)
        .join(AGENT_HOST)
        .join("operator.jsonl");
    let hidden = file.with_file_name(".operator.jsonl.icloud");
    std::fs::rename(&file, &hidden).expect("evict");
    refused(
        world.mobile.snooze(need(OTHER_ASK), hour()),
        RefusalCode::NotSynced,
    );
    std::fs::rename(&hidden, &file).expect("download");

    // refresh_timeout: another reader of the mobile client's index holds the ingest
    // lock and the Vault has a line the mobile client has not read.
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
        "00000000000000000000000042",
        &format!("s-{}", session(102)),
        "pulse",
        json!({"node": "n1", "attempt": "00000000000000000000000012"}),
        json!({"agent": agent(102), "message": "Working.", "boundary": false}),
    );
    let started = std::time::Instant::now();
    refused(
        world.mobile.snooze(need(OTHER_ASK), hour()),
        RefusalCode::RefreshTimeout,
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    assert!(world.mobile_lines().is_empty(), "a refusal writes nothing");

    held.unlock().expect("release");
    world
        .mobile
        .snooze(need(OTHER_ASK), hour())
        .expect("the same request passes once the index is free");
    assert_eq!(world.mobile_lines().len(), 1);
}

#[test]
fn a_need_with_a_default_returns_fifteen_minutes_before_it() {
    let world = World::new();
    let with_default = world.ask_with_default(50, 2 * HOUR);
    let row = |board: &NowBoard, id: &str| {
        let rows = board.needs.iter().chain(&board.snoozed);
        rows.clone()
            .find(|row| row.id == id)
            .unwrap_or_else(|| panic!("no row {id}"))
            .clone()
    };
    let waiting = row(&world.board(), &with_default);
    let default_at =
        UtcTime::parse(waiting.default_at.as_deref().expect("a default time")).expect("default_at");
    let back = default_at.add_seconds(-15 * 60);

    // Tomorrow is past the default: the mobile client writes the cut time and reads it.
    world
        .mobile
        .snooze(with_default.clone(), in_seconds(DAY).canonical())
        .expect("snooze");
    let lines = world.mobile_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["body"]["until"], json!(back));
    let board = world.board();
    assert_eq!(ids(&board.snoozed), [with_default.as_str()]);
    assert_eq!(
        board.snoozed[0].snoozed_until.as_deref(),
        Some(back.canonical().as_str())
    );
    assert_eq!(world.mac_sees(&with_default), Some(back));
    let page = world
        .mobile
        .project(PROJECT.to_owned())
        .expect("the project page");
    let on_page = page
        .needs
        .iter()
        .find(|row| row.id == with_default)
        .expect("the page lists the snoozed need");
    assert_eq!(
        on_page.snoozed_until.as_deref(),
        Some(back.canonical().as_str())
    );

    // A line an older build wrote with the owner's own time is cut on read.
    world.line(
        "00000000000000000000000060",
        "operator",
        "need.snoozed",
        json!({}),
        json!({"need": with_default, "revision": waiting.revision,
               "until": in_seconds(3 * DAY).canonical()}),
    );
    let board = world.board();
    assert_eq!(
        row(&board, &with_default).snoozed_until.as_deref(),
        Some(back.canonical().as_str())
    );
    // A need with no default is hidden as long as asked.
    let week = in_seconds(6 * DAY);
    world
        .mobile
        .snooze(need(ASK), week.canonical())
        .expect("snooze");
    assert_eq!(
        row(&world.board(), &need(ASK)).snoozed_until.as_deref(),
        Some(week.canonical().as_str())
    );
}

#[test]
fn a_default_under_fifteen_minutes_away_refuses_the_snooze() {
    let world = World::new();
    let near = world.ask_with_default(50, 10 * 60);
    world.mobile.refresh(5_000).expect("refresh");
    invalid(
        world
            .mobile
            .snooze(near.clone(), in_seconds(HOUR).canonical()),
        "snooze_default_near",
    );
    assert!(world.mobile_lines().is_empty(), "a refusal writes nothing");
    // The seven-day limit still answers for such a need.
    invalid(
        world
            .mobile
            .snooze(near.clone(), in_seconds(7 * DAY + 60).canonical()),
        "snooze_too_long",
    );
    assert_eq!(ids(&world.board().snoozed), [] as [&str; 0]);
}
