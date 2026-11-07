//! Shared wire fixtures for integration tests.
//!
//! Each integration test binary compiles this module independently and uses a
//! different subset, so unused helpers here are expected rather than dead code.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use agendar_core::model::{Event, SourcePath, parse_event};
use serde_json::{Value, json};

pub const PROJECT: &str = "example";
pub const HOST: &str = "h-00000000000000000000000001";
pub const HOST2: &str = "h-00000000000000000000000002";
pub const SESSION: &str = "00000000000000000000000101";
pub const EVENT_ID: &str = "00000000000000000000000010";
pub const REF_ID: &str = "00000000000000000000000099";
pub const DIGEST: &str = "49b2ac2c2826cfadc7f9ba7de8f550e4800afa0be6a5f0b1b10806d86b7b9203";

pub const OPERATOR: &str = "h-00000000000000000000000001/operator";
pub const STEWARD: &str = "h-00000000000000000000000001/steward";
pub const AGENT: &str = "h-00000000000000000000000001/s-00000000000000000000000101";
pub const AGENT2: &str = "h-00000000000000000000000001/s-00000000000000000000000102";

const TS: &str = "2026-01-01T00:00:00.000Z";

pub fn source_for(project: &str, writer: &str) -> SourcePath {
    SourcePath::from_relative_path(&format!("projects/{project}/{writer}.jsonl"))
        .expect("fixture source path must be valid")
}

pub fn line(value: &Value) -> String {
    let mut text = serde_json::to_string(value).expect("fixture JSON must serialize");
    text.push('\n');
    text
}

pub fn parse_value(value: &Value) -> agendar_core::Result<Event> {
    let project = value["project"].as_str().expect("fixture project");
    let writer = value["writer"].as_str().expect("fixture writer");
    let source = source_for(project, writer);
    parse_event(line(value).as_bytes(), &source)
}

pub fn parse_value_with_source(value: &Value, source: &SourcePath) -> agendar_core::Result<Event> {
    parse_event(line(value).as_bytes(), source)
}

pub fn event(kind: &str, body: Value, refs: Value, writer: &str) -> Value {
    json!({
        "v": 1,
        "id": EVENT_ID,
        "seq": 1,
        "writer": writer,
        "ts": TS,
        "project": PROJECT,
        "refs": refs,
        "kind": kind,
        "body": body,
    })
}

pub fn agent() -> Value {
    json!({"session": SESSION, "harness": "generic"})
}

pub fn blob() -> Value {
    json!({"host": HOST, "sha256": DIGEST, "ext": "txt", "bytes": 21})
}

pub fn blob_with_ext(ext: &str) -> Value {
    json!({"host": HOST, "sha256": DIGEST, "ext": ext, "bytes": 21})
}

pub fn criterion() -> Value {
    json!({"id": "done", "text": "It is finished."})
}

pub fn criterion_result() -> Value {
    json!({
        "criterion": "done",
        "status": "passed",
        "claim": "agent_says",
        "evidence": blob(),
    })
}

pub fn policy_eval() -> Value {
    json!({
        "requested": "ask",
        "effective": "ask",
        "reversibility": "easy",
        "external": false,
        "heuristic_hits": [],
        "budget": {
            "speculative_features": 0,
            "pending_slices": 0,
            "demo_gap_s": 0,
            "blocked": false,
            "reasons": [],
        },
    })
}

pub fn policy_set() -> Value {
    json!({
        "preset": "balanced",
        "cells": {
            "easy_local": "auto",
            "easy_wide": "assume",
            "costly_local": "assume",
            "costly_wide": "ask",
            "irreversible_local": "ask",
            "irreversible_wide": "ask",
        },
        "domain_floor": {},
        "budget": {
            "max_speculative_features": 10,
            "max_pending_slices": 2,
            "max_demo_gap_s": 14400,
        },
        "require_human_review": false,
        "allow_timeouts": true,
    })
}

pub fn card() -> Value {
    json!({
        "why_now": "Wake delivery is blocked.",
        "question": "How should answers reach idle agents?",
        "reversibility": "easy",
        "reach": "local",
        "external": false,
        "domain": "arch",
        "options": [
            {
                "id": "a",
                "label": "Choose A",
                "consequence": "Do the first thing.",
                "reversibility": "easy",
                "external": false,
            },
            {
                "id": "b",
                "label": "Choose B",
                "consequence": "Do the second thing.",
                "reversibility": "easy",
                "external": false,
            },
        ],
        "recommended": {"option": "a", "confidence": "med", "because": "It is simpler."},
        "already_checked": [],
        "done_when": [criterion()],
        "on_timeout": {"kind": "hold"},
    })
}

pub fn delivery_body() -> Value {
    json!({
        "agent": agent(),
        "name": "Example bytes",
        "demos": [{"kind": "cli_transcript", "body": {"blob": blob()}}],
        "criteria": [criterion_result()],
        "disclosure": {
            "proven": ["Agent compared bytes"],
            "not_proven_yet": [],
            "faked_for_now": [],
        },
    })
}

pub fn evidence_body() -> Value {
    json!({
        "criterion": "done",
        "method": {
            "kind": "command",
            "check_id": "fixture-equality",
            "argv_digest": DIGEST,
        },
        "started_at": "2026-01-01T00:00:00.000Z",
        "finished_at": "2026-01-01T00:00:01.000Z",
        "result": "passed",
        "digest": DIGEST,
        "output": blob(),
        "approval_id": "fixture-check",
    })
}

pub fn delivered_body() -> Value {
    json!({
        "session": SESSION,
        "packet_id": DIGEST,
        "transport": "generic",
        "outcome": "accepted",
    })
}

pub fn valid_event(kind: &str) -> Value {
    let wire_kind = match kind {
        "project.declared.agent" => "project.declared",
        "plan.feature.agent" => "plan.feature",
        "work.progress.started"
        | "work.progress.done"
        | "work.progress.verified"
        | "work.progress.dropped" => "work.progress",
        "pulse.work" | "pulse.session" => "pulse",
        "delivered.answer" | "delivered.review" => "delivered",
        other => other,
    };
    match kind {
        "project.declared" => event(wire_kind, json!({"title": "Example"}), json!({}), OPERATOR),
        "project.declared.agent" => event(
            wire_kind,
            json!({"agent": agent(), "title": "Example"}),
            json!({}),
            AGENT,
        ),
        "plan.feature" => event(
            wire_kind,
            json!({"title": "x"}),
            json!({"node": "x"}),
            OPERATOR,
        ),
        "plan.feature.agent" => event(
            wire_kind,
            json!({"agent": agent(), "title": "x"}),
            json!({"node": "x"}),
            AGENT,
        ),
        "work.progress.started" => event(
            wire_kind,
            json!({
                "agent": agent(),
                "status": "started",
                "eta": {"min_s": 60, "max_s": 120},
                "evidence": [],
            }),
            json!({"node": "x"}),
            AGENT,
        ),
        "work.progress.done" => event(
            wire_kind,
            json!({"agent": agent(), "status": "done", "evidence": []}),
            json!({"node": "x", "attempt": REF_ID}),
            AGENT,
        ),
        "work.progress.verified" => event(
            wire_kind,
            json!({
                "agent": agent(),
                "status": "verified",
                "evidence": [criterion_result()],
            }),
            json!({"node": "x", "attempt": REF_ID}),
            AGENT,
        ),
        "work.progress.dropped" => event(
            wire_kind,
            json!({"agent": agent(), "status": "dropped", "evidence": []}),
            json!({"node": "x", "attempt": REF_ID}),
            AGENT,
        ),
        "pulse.work" => event(
            wire_kind,
            json!({"agent": agent(), "message": "Working.", "boundary": false}),
            json!({"node": "x", "attempt": REF_ID}),
            AGENT,
        ),
        "pulse.session" => event(
            wire_kind,
            json!({"agent": agent(), "message": "Started.", "boundary": false}),
            json!({}),
            AGENT,
        ),
        "decision.ask" => event(
            wire_kind,
            json!({"agent": agent(), "card": card(), "policy": policy_eval()}),
            json!({"node": "x"}),
            AGENT,
        ),
        "decision.assumed" => event(
            wire_kind,
            json!({
                "agent": agent(),
                "option": "a",
                "reason": "policy",
                "policy": policy_eval(),
            }),
            json!({"node": "x", "ask": REF_ID}),
            AGENT,
        ),
        "decision.auto" => event(
            wire_kind,
            json!({
                "agent": agent(),
                "option": "a",
                "reason": "policy",
                "policy": policy_eval(),
            }),
            json!({"node": "x", "ask": REF_ID}),
            AGENT,
        ),
        "decision.withdrawn" => event(
            wire_kind,
            json!({"agent": agent(), "reason": "No longer needed."}),
            json!({"node": "x", "ask": REF_ID}),
            AGENT,
        ),
        "decision.answer" => event(
            wire_kind,
            json!({"option": "a", "done_when": [criterion()]}),
            json!({"node": "x", "ask": REF_ID}),
            OPERATOR,
        ),
        "decision.ack" => event(
            wire_kind,
            json!({"agent": agent(), "applied_option": "a", "restarted": []}),
            json!({"node": "x", "ask": REF_ID, "answer": REF_ID}),
            AGENT,
        ),
        "delivery" => event(
            wire_kind,
            delivery_body(),
            json!({"node": "x", "attempt": REF_ID, "receipt": REF_ID}),
            AGENT,
        ),
        "evidence.observed" => event(
            wire_kind,
            evidence_body(),
            json!({"node": "x", "delivery": REF_ID}),
            STEWARD,
        ),
        "review.result" => event(
            wire_kind,
            json!({"verdict": "accept"}),
            json!({"node": "x", "delivery": REF_ID}),
            OPERATOR,
        ),
        "rule.set" => event(
            wire_kind,
            json!({"rule_id": "no-shell", "text": "Use explicit argv.", "enabled": true}),
            json!({}),
            OPERATOR,
        ),
        "policy.set" => event(wire_kind, policy_set(), json!({}), OPERATOR),
        "session.lost" => event(
            wire_kind,
            json!({"session": SESSION, "process_start": 1, "reason": "exited"}),
            json!({}),
            STEWARD,
        ),
        "session.continued" => event(
            wire_kind,
            json!({"session": SESSION, "previous": "00000000000000000000000102"}),
            json!({}),
            STEWARD,
        ),
        "delivered.answer" => event(
            wire_kind,
            delivered_body(),
            json!({"node": "x", "ask": REF_ID, "answer": REF_ID}),
            STEWARD,
        ),
        "delivered.review" => event(
            wire_kind,
            delivered_body(),
            json!({"node": "x", "delivery": REF_ID, "causes": [REF_ID]}),
            STEWARD,
        ),
        other => panic!("unknown fixture kind {other}"),
    }
}

/// One approved seed package under `tests/oracle/seeds/<name>`.
pub struct Seed {
    pub name: String,
    pub dir: PathBuf,
    pub events: Vec<Event>,
    pub as_of: agendar_core::model::UtcTime,
    pub aliases: BTreeMap<String, String>,
    pub expected: Value,
}

pub fn seed_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/oracle/seeds")
        .join(name)
}

/// SHA-256 of one file's bytes, using the same typed digest as the protocol.
pub fn file_digest(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("seed file must be readable");
    agendar_core::model::Sha256::from_bytes(&bytes)
        .as_str()
        .to_owned()
}

pub fn load_seed(name: &str) -> Seed {
    let dir = seed_dir(name);
    let context: Value = serde_json::from_slice(
        &std::fs::read(dir.join("context.json")).expect("seed context must exist"),
    )
    .expect("seed context must be JSON");
    let as_of =
        agendar_core::model::UtcTime::parse(context["as_of"].as_str().expect("seed context as_of"))
            .expect("seed as_of must be canonical");
    let aliases_json: Value = serde_json::from_slice(
        &std::fs::read(dir.join("aliases.json")).expect("seed aliases must exist"),
    )
    .expect("seed aliases must be JSON");
    let mut aliases = BTreeMap::new();
    for (alias, id) in aliases_json.as_object().expect("seed aliases object") {
        aliases.insert(alias.clone(), id.as_str().expect("alias id").to_owned());
    }
    let expected: Value = serde_json::from_slice(
        &std::fs::read(dir.join("expected.json")).expect("seed expected must exist"),
    )
    .expect("seed expected must be JSON");
    let text = std::fs::read_to_string(dir.join("events.jsonl")).expect("seed events must exist");
    let mut events = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line).expect("seed event must be JSON");
        let project = value["project"].as_str().expect("seed event project");
        let writer = value["writer"].as_str().expect("seed event writer");
        let source = SourcePath::from_relative_path(&format!("projects/{project}/{writer}.jsonl"))
            .expect("seed source path");
        let mut complete = line.as_bytes().to_vec();
        complete.push(b'\n');
        events.push(parse_event(&complete, &source).expect("seed event must decode"));
    }
    Seed {
        name: name.to_owned(),
        dir,
        events,
        as_of,
        aliases,
        expected,
    }
}

impl Seed {
    pub fn alias(&self, name: &str) -> &str {
        self.aliases
            .get(name)
            .unwrap_or_else(|| panic!("seed {} has alias {name}", self.name))
            .as_str()
    }

    pub fn assertions(&self) -> &Value {
        &self.expected["assertions"]
    }
}

/// One implementation fixture under `crates/agendar-core/tests/fixtures/<name>`.
pub struct Fixture {
    pub name: String,
    pub dir: PathBuf,
    pub events: Vec<Event>,
    pub as_of: agendar_core::model::UtcTime,
    pub expected: Option<Value>,
}

pub fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn fixture_names() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut names = Vec::new();
    for entry in std::fs::read_dir(root).expect("fixtures directory exists") {
        let entry = entry.expect("fixture entry");
        if entry.file_type().expect("fixture type").is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    names
}

pub fn load_fixture(name: &str) -> Fixture {
    let dir = fixture_dir(name);
    let context: Value = serde_json::from_slice(
        &std::fs::read(dir.join("context.json")).expect("fixture context must exist"),
    )
    .expect("fixture context must be JSON");
    let as_of =
        agendar_core::model::UtcTime::parse(context["as_of"].as_str().expect("fixture as_of"))
            .expect("fixture as_of must be canonical");
    let text =
        std::fs::read_to_string(dir.join("events.jsonl")).expect("fixture events must exist");
    assert!(
        text.ends_with('\n'),
        "fixture {} events.jsonl must be newline terminated",
        name
    );
    let mut events = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line).expect("fixture event must be JSON");
        let project = value["project"].as_str().expect("fixture event project");
        let writer = value["writer"].as_str().expect("fixture event writer");
        let source = SourcePath::from_relative_path(&format!("projects/{project}/{writer}.jsonl"))
            .expect("fixture source path");
        let mut complete = line.as_bytes().to_vec();
        complete.push(b'\n');
        events.push(parse_event(&complete, &source).expect("fixture event must decode"));
    }
    let expected = std::fs::read(dir.join("expected.json"))
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).expect("fixture expected must be JSON"));
    Fixture {
        name: name.to_owned(),
        dir,
        events,
        as_of,
        expected,
    }
}

/// Append one encoded event line to its Vault writer file.
pub fn append_event_to_vault(
    vault: &agendar_core::vault::Vault,
    event: &Event,
    segment: u32,
) -> agendar_core::Result<()> {
    let line = serde_json::to_string(event).expect("event serializes");
    append_raw_line(
        vault,
        &event.project,
        &event.writer,
        segment,
        &format!("{line}\n"),
    )
}

/// Append arbitrary bytes to one writer file, creating its directories.
pub fn append_raw_line(
    vault: &agendar_core::vault::Vault,
    project: &agendar_core::model::ProjectSlug,
    writer: &agendar_core::model::WriterId,
    segment: u32,
    line: &str,
) -> agendar_core::Result<()> {
    use std::io::Write as _;
    let directory =
        agendar_core::vault::ensure_host_directory(vault.root(), project, writer.host())?;
    let name = agendar_core::vault::writer_file_name(writer, segment)?;
    let path = directory.join(name);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    file.flush()?;
    Ok(())
}
