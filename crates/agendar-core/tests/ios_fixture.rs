//! The iOS test Vault (ignored; `scripts/ios-build.sh` runs it):
//! `AGENDAR_IOS_FIXTURE=<new dir> cargo test -p agendar-core --test ios_fixture -- --ignored --nocapture`
//!
//! Writes a new Vault holding one project of about 5,000 events from three
//! agent hosts (the `reduce_scaling` workload) with three open asks of two
//! options, one per answer guard (the workload's own is the easy one; `extras`
//! adds a choose and a confirm card), and a contested step whose rescue row
//! asks who continues. The workload's next 60 features give five sync batches of 50
//! events under `AGENDAR_IOS_SYNC_BATCHES`, each laid out as Vault-relative
//! files whose lines a reader appends, as if they had synced from the Mac. The last batched event is ten minutes old. Without the
//! variables the test writes to a temporary directory and only checks the shape.

mod support;
mod workload;

use std::path::{Path, PathBuf};

use agendar_core::guard::{Guard, guard_for};
use agendar_core::model::{Event, UtcTime};
use agendar_core::reduce::{ReduceContext, reduce_resolved, resolve};
use agendar_core::vault::{Clock, SystemClock, UlidIdSource, Vault, writer_relative_path};

/// About five events per feature: the Vault holds the first `VAULT_FEATURES`,
/// the sync batches the rest.
const VAULT_FEATURES: usize = 1022;
const FEATURES: usize = VAULT_FEATURES + 60;
const BATCHES: usize = 5;
const BATCH_EVENTS: usize = 50;
const AGENT_HOSTS: usize = 3;
/// The one open ask, late in the chain so most of the project has moved.
const OPEN_ASK: usize = 980;

fn shape(base: UtcTime) -> workload::Shape {
    workload::Shape {
        features: FEATURES,
        deep: false,
        agent_hosts: AGENT_HOSTS,
        open_ask: Some(OPEN_ASK),
        base,
    }
}

/// What the workload does not hold: a costly ask (choose), an irreversible ask
/// (confirm) and a step two agents started at once (a "who continues" rescue
/// row). Written by four new sessions on the second agent host, each planning
/// its own step, so no existing writer's sequence moves.
fn extras(after: UtcTime) -> Vec<Event> {
    use serde_json::json;
    let host = workload::host(1);
    let mut events = Vec::new();
    let mut next_id = 8_000_000_u64;
    let mut seqs = std::collections::BTreeMap::<u64, u64>::new();
    let mut push = |session: u64, kind: &str, node: &str, body: serde_json::Value| {
        next_id += 1;
        let seq = seqs.entry(session).or_insert(0);
        *seq += 1;
        let value = json!({
            "v": 1, "id": format!("{next_id:026}"), "seq": *seq,
            "writer": format!("{host}/s-{session:026}"),
            "ts": after.add_seconds((next_id - 8_000_000) as i64).canonical(),
            "project": workload::PROJECT, "refs": {"node": node}, "kind": kind, "body": body,
        });
        events.push(support::parse_value(&value).expect("extra event parses"));
    };
    let agent = |session: u64| json!({"harness": "generic", "session": format!("{session:026}")});
    let plan = |session: u64, title: &str| {
        json!({"agent": agent(session), "demo_kind": "cli_transcript", "depends_on": [],
               "slice": "s200", "title": title})
    };
    let started = |session: u64| json!({"agent": agent(session), "eta": {"max_s": 900, "min_s": 300}, "evidence": [], "status": "started"});
    let ask = |session: u64, risk: &str, question: &str, first: [&str; 2], second: [&str; 2]| {
        json!({"agent": agent(session), "card": {
            "already_checked": [], "domain": "arch",
            "done_when": [{"id": "bytes", "text": "Demo bytes match the approved digest."}],
            "external": false, "on_timeout": {"kind": "hold"},
            "options": [
                {"consequence": first[1], "eta": {"max_s": 3600, "min_s": 1800}, "external": false,
                 "icon": "link", "id": "first", "label": first[0], "reversibility": risk},
                {"consequence": second[1], "eta": {"max_s": 1800, "min_s": 900}, "external": false,
                 "icon": "inbox", "id": "second", "label": second[0], "reversibility": risk}
            ],
            "question": question, "reach": "local",
            "recommended": {"because": "It keeps the plan short.", "confidence": "high", "option": "first"},
            "reversibility": risk, "why_now": "The step cannot go on without it."},
          "policy": {"budget": {"blocked": false, "demo_gap_s": 0, "pending_slices": 0, "reasons": [],
                                "speculative_features": 0},
                     "effective": "ask", "external": false, "heuristic_hits": [], "requested": "ask",
                     "reversibility": risk}})
    };
    let (choose, confirm, first, second) = (9_900_001, 9_900_002, 9_900_003, 9_900_004);
    push(
        choose,
        "plan.feature",
        "n2001",
        plan(choose, "Store the search index"),
    );
    push(choose, "work.progress", "n2001", started(choose));
    push(
        choose,
        "decision.ask",
        "n2001",
        ask(
            choose,
            "costly",
            "Which format should the search index use?",
            [
                "One file per project",
                "Simple, and slower past a few thousand steps.",
            ],
            [
                "One shared file",
                "Fast, and a format change means a rebuild.",
            ],
        ),
    );
    push(
        confirm,
        "plan.feature",
        "n2002",
        plan(confirm, "Retire the old layout"),
    );
    push(confirm, "work.progress", "n2002", started(confirm));
    push(
        confirm,
        "decision.ask",
        "n2002",
        ask(
            confirm,
            "irreversible",
            "Should the old layout go for good?",
            [
                "Keep both layouts",
                "Nothing is lost, and both stay to maintain.",
            ],
            [
                "Retire the old layout",
                "One layout to maintain, and no way back.",
            ],
        ),
    );
    push(
        first,
        "plan.feature",
        "n2003",
        plan(first, "Tidy the sample data"),
    );
    push(first, "work.progress", "n2003", started(first));
    push(second, "work.progress", "n2003", started(second));
    events
}

fn bytes_under(path: &Path) -> u64 {
    std::fs::read_dir(path)
        .expect("read dir")
        .map(|entry| {
            let entry = entry.expect("dir entry");
            let kind = entry.file_type().expect("file type");
            if kind.is_dir() {
                bytes_under(&entry.path())
            } else {
                entry.metadata().expect("metadata").len()
            }
        })
        .sum()
}

#[test]
#[ignore = "writes the iOS fixture Vault; run by scripts/ios-build.sh"]
fn writes_the_ios_fixture_vault() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = std::env::var_os("AGENDAR_IOS_FIXTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| temp.path().join("vault"));
    // The last batched event is ten minutes before now: one dry run measures the span.
    let now = SystemClock.now();
    let (dry, as_of) = workload::workload(&shape(now));
    let span = (as_of.millis() - now.millis()) / 1000;
    let (events, _) = workload::workload(&shape(now.add_seconds(-span)));
    assert_eq!(dry.len(), events.len());

    let batches_root = std::env::var_os("AGENDAR_IOS_SYNC_BATCHES")
        .map(PathBuf::from)
        .unwrap_or_else(|| temp.path().join("batches"));

    // Cut where feature `VAULT_FEATURES` is declared: each earlier feature's
    // ask, answer and ack stay together in the Vault.
    let cut_node = format!("n{VAULT_FEATURES}");
    let cut = events
        .iter()
        .position(|event| {
            let value = serde_json::to_value(event).expect("event serializes");
            value["kind"] == "plan.feature" && value["refs"]["node"] == cut_node.as_str()
        })
        .expect("cut feature");
    let (events, tail) = events.split_at(cut);
    let mut events = events.to_vec();
    events.extend(extras(events.last().expect("events").ts));
    let events = events.as_slice();
    assert!(
        tail.len() >= BATCHES * BATCH_EVENTS,
        "{} tail events",
        tail.len()
    );

    let vault = Vault::init(&root, &SystemClock, &mut UlidIdSource).expect("vault init");
    for event in events {
        support::append_event_to_vault(&vault, event, 1).expect("append event");
    }
    for (batch, chunk) in tail.chunks(BATCH_EVENTS).take(BATCHES).enumerate() {
        let dir = batches_root.join(format!("batch-{}", batch + 1));
        for event in chunk {
            let relative =
                writer_relative_path(&event.project, &event.writer, 1).expect("relative path");
            let path = dir.join(relative);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("batch dir");
            let mut text = std::fs::read_to_string(&path).unwrap_or_default();
            text.push_str(&serde_json::to_string(event).expect("event serializes"));
            text.push('\n');
            std::fs::write(&path, text).expect("batch file");
        }
    }

    let resolution = resolve(events);
    let state = reduce_resolved(&resolution, events, &ReduceContext { as_of: now });
    let project = state.projects.values().next().expect("one project");
    let open: Vec<_> = project
        .needs_you
        .iter()
        .filter(|need| need.kind.as_str() == "decide")
        .map(|need| &project.decisions[&need.source])
        .collect();
    let guard_of = |guard: Guard| {
        let mut with = open.iter().filter(|ask| guard_for(&ask.card) == guard);
        let ask = with
            .next()
            .unwrap_or_else(|| panic!("an open {guard:?} ask"));
        assert!(with.next().is_none(), "exactly one open {guard:?} ask");
        assert_eq!(ask.card.options.len(), 2);
        *ask
    };
    assert_eq!(open.len(), 3, "three open asks");
    let ask = guard_of(Guard::Easy);
    let (choose, confirm) = (guard_of(Guard::Choose), guard_of(Guard::Confirm));
    let rescue = project
        .needs_you
        .iter()
        .find(|need| need.id.starts_with("rescue:start-conflict:"))
        .expect("a contested step");
    assert_eq!(
        rescue.node.as_ref().map(|node| node.as_str()),
        Some("n2003")
    );
    println!(
        "ios-fixture: {} events, {} bytes, {} hosts, {BATCHES} sync batches of {BATCH_EVENTS} events, open ask {} (options {}) at {}",
        events.len(),
        bytes_under(&root),
        AGENT_HOSTS,
        ask.id.as_str(),
        ask.card
            .options
            .iter()
            .map(|option| option.id.as_str())
            .collect::<Vec<_>>()
            .join(","),
        root.display(),
    );
    println!(
        "ios-fixture: choose ask {}, confirm ask {}, rescue row {}",
        choose.id.as_str(),
        confirm.id.as_str(),
        rescue.id,
    );
}
