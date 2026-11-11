//! The synthetic one-project workload shared by the reducer scaling benchmark
//! (`reduce_scaling.rs`) and the iOS fixture Vault (`ios_fixture.rs`).
//!
//! One project with a dependency chain, re-planned dependencies, a rejected
//! cycle, sessions that turn over, asks with answers and acks, answer
//! deliveries, deliveries with observations and reviews.
#![allow(dead_code)]

use agendar_core::model::{Event, UtcTime};
use serde_json::{Value, json};

use crate::support;

pub const HOST: &str = "h-00000000000000000000000001";
const OPERATOR: &str = "h-00000000000000000000000001/operator";
const STEWARD: &str = "h-00000000000000000000000001/steward";
pub const PROJECT: &str = "bench";

/// The workload's shape. [`Shape::bench`] is the benchmark's historical shape,
/// which `reduce_scaling` digests must keep reproducing.
pub struct Shape {
    /// Features (about 5 events each).
    pub features: usize,
    /// One long chain broken only at slice starts, so an ask holds many
    /// downstream features; otherwise the chain breaks every third feature.
    pub deep: bool,
    /// Agent sessions are spread over this many hosts (`h-…01`, `h-…02`, …).
    pub agent_hosts: usize,
    /// `None`: every 64th feature asks and is never answered. `Some(index)`:
    /// only that feature leaves its ask open.
    pub open_ask: Option<usize>,
    /// Timestamp of the first event; each event is 7 s after the previous.
    pub base: UtcTime,
}

impl Shape {
    pub fn bench(features: usize, deep: bool) -> Self {
        Self {
            features,
            deep,
            agent_hosts: 1,
            open_ask: None,
            base: UtcTime::parse("2026-01-01T00:00:00.000Z").expect("base"),
        }
    }
}

/// The host id for agent host `index` (0-based): `h-…01` is the operator's.
pub fn host(index: usize) -> String {
    format!("h-{:026}", index + 1)
}

struct Gen {
    next_id: u64,
    tick: i64,
    base: UtcTime,
    seqs: std::collections::BTreeMap<String, u64>,
    events: Vec<Event>,
}

impl Gen {
    fn new(base: UtcTime) -> Self {
        Self {
            next_id: 1000,
            tick: 0,
            base,
            seqs: std::collections::BTreeMap::new(),
            events: Vec::new(),
        }
    }

    fn push(&mut self, kind: &str, writer: &str, refs: Value, body: Value) -> String {
        self.next_id += 1;
        self.tick += 7;
        let id = format!("{:026}", self.next_id);
        let seq = self.seqs.entry(writer.to_owned()).or_insert(0);
        *seq += 1;
        let value = json!({
            "v": 1,
            "id": id,
            "seq": *seq,
            "writer": writer,
            "ts": self.base.add_seconds(self.tick).canonical(),
            "project": PROJECT,
            "refs": refs,
            "kind": kind,
            "body": body,
        });
        self.events
            .push(support::parse_value(&value).expect("synthetic event parses"));
        id
    }
}

fn blob() -> Value {
    json!({"host": HOST, "sha256": support::DIGEST, "ext": "txt", "bytes": 21})
}

fn card() -> Value {
    json!({
        "already_checked": [],
        "domain": "arch",
        "done_when": [{"id": "bytes", "text": "Demo bytes match the approved digest."}],
        "external": false,
        "on_timeout": {"kind": "hold"},
        "options": [
            {"consequence": "Use hooks.", "eta": {"max_s": 3600, "min_s": 1800}, "external": false,
             "icon": "link", "id": "adapters", "label": "Use harness adapters", "reversibility": "easy"},
            {"consequence": "Use the inbox.", "eta": {"max_s": 1800, "min_s": 900}, "external": false,
             "icon": "inbox", "id": "inbox", "label": "Use inbox on next start", "reversibility": "easy"}
        ],
        "question": "How should answers reach idle agents?",
        "reach": "local",
        "recommended": {"because": "It is simpler.", "confidence": "med", "option": "adapters"},
        "reversibility": "easy",
        "why_now": "Wake delivery is blocked until an approach is chosen."
    })
}

fn policy() -> Value {
    json!({
        "budget": {"blocked": false, "demo_gap_s": 0, "pending_slices": 0, "reasons": [], "speculative_features": 0},
        "effective": "ask", "external": false, "heuristic_hits": [], "requested": "ask", "reversibility": "easy"
    })
}

fn node(index: usize) -> String {
    format!("n{index}")
}

/// Build one project of `shape`; returns its events and an `as_of` 10 minutes
/// after the last event.
pub fn workload(shape: &Shape) -> (Vec<Event>, UtcTime) {
    let features = shape.features;
    let deep = shape.deep;
    let mut generator = Gen::new(shape.base);
    generator.push(
        "project.declared",
        OPERATOR,
        json!({}),
        json!({"brief": "Synthetic", "goal": "Measure", "title": "Bench"}),
    );
    for index in 0..features {
        let mut depends_on: Vec<String> = Vec::new();
        // `deep`: one long chain broken only at slice starts, so an ask holds
        // many downstream features; the default breaks it every third feature.
        let chained = if deep {
            index % 10 != 0
        } else {
            index % 3 != 0
        };
        if chained && index > 0 {
            depends_on.push(node(index - 1));
        }
        if index >= 5 && index % 4 == 0 {
            depends_on.push(node(index - 5));
        }
        // A forward edge that a later feature closes into a cycle.
        if index % 97 == 1 {
            depends_on.push(node(index + 1));
        }
        generator.push(
            "plan.feature",
            OPERATOR,
            json!({"node": node(index)}),
            json!({"demo_kind": "cli_transcript", "depends_on": depends_on,
                   "slice": format!("s{}", index / 10), "title": format!("Feature {index} with a realistic title")}),
        );
        let session = format!("{:026}", 9_000_000 + index / 25);
        let agent_host = host((index / 25) % shape.agent_hosts.max(1));
        let writer = format!("{agent_host}/s-{session}");
        let agent = json!({"harness": "generic", "session": session});
        let started = generator.push(
            "work.progress",
            &writer,
            json!({"node": node(index)}),
            json!({"agent": agent, "eta": {"max_s": 900, "min_s": 300}, "evidence": [], "status": "started"}),
        );
        generator.push(
            "pulse",
            &writer,
            json!({"node": node(index), "attempt": started}),
            json!({"agent": agent, "message": "Working.", "boundary": false}),
        );
        let mut receipt: Option<String> = None;
        // Every 64th feature (or only `shape.open_ask`) asks and is never
        // answered: an open human wait that holds its dependents until the end.
        let unanswered = match shape.open_ask {
            Some(open) => index == open,
            None => index % 64 == 32,
        };
        if unanswered {
            generator.push(
                "decision.ask",
                &writer,
                json!({"node": node(index)}),
                json!({"agent": agent, "card": card(), "policy": policy()}),
            );
        }
        if index % 8 == 0 {
            let ask = generator.push(
                "decision.ask",
                &writer,
                json!({"node": node(index)}),
                json!({"agent": agent, "card": card(), "policy": policy()}),
            );
            let answer = generator.push(
                "decision.answer",
                OPERATOR,
                json!({"node": node(index), "ask": ask}),
                json!({"done_when": [{"id": "bytes", "text": "Demo bytes match the approved digest."}],
                       "option": "adapters"}),
            );
            generator.push(
                "delivered",
                STEWARD,
                json!({"node": node(index), "ask": ask, "answer": answer}),
                json!({"session": session, "packet_id": support::DIGEST, "transport": "generic", "outcome": "accepted"}),
            );
            generator.push(
                "decision.ack",
                &writer,
                json!({"node": node(index), "ask": ask, "answer": answer}),
                json!({"agent": agent, "applied_option": "adapters", "restarted": []}),
            );
            receipt = Some(answer);
        }
        // Every 50th feature drops one dependency later (a re-plan transition).
        if index % 50 == 10 {
            generator.push(
                "plan.feature",
                OPERATOR,
                json!({"node": node(index - 1)}),
                json!({"demo_kind": "cli_transcript", "depends_on": [],
                       "slice": format!("s{}", (index - 1) / 10), "title": format!("Feature {} re-planned", index - 1)}),
            );
        }
        // The cycle closer: the feature after a forward edge depends back.
        if index % 97 == 2 {
            generator.push(
                "plan.feature",
                OPERATOR,
                json!({"node": node(index)}),
                json!({"demo_kind": "cli_transcript", "depends_on": [node(index - 1)],
                       "slice": format!("s{}", index / 10), "title": format!("Feature {index} closes a cycle")}),
            );
        }
        let open_tail = index + 3 >= features || unanswered;
        if !open_tail {
            if index % 10 == 0 {
                let mut refs = json!({"node": node(index), "attempt": started});
                let mut criteria = json!([]);
                if let Some(receipt) = &receipt {
                    refs["receipt"] = json!(receipt);
                    criteria = json!([{"claim": "agent_says", "criterion": "bytes", "evidence": blob(), "status": "passed"}]);
                }
                let delivery = generator.push(
                    "delivery",
                    &writer,
                    refs,
                    json!({"agent": agent, "name": format!("Delivery {index}"),
                           "demos": [{"kind": "cli_transcript", "body": {"blob": blob()}}],
                           "criteria": criteria,
                           "disclosure": {"proven": ["Agent compared bytes"], "not_proven_yet": [], "faked_for_now": []}}),
                );
                let observed = generator.push(
                    "evidence.observed",
                    STEWARD,
                    json!({"node": node(index), "delivery": delivery, "causes": []}),
                    json!({"criterion": "bytes",
                           "method": {"kind": "command", "check_id": "fixture-equality", "argv_digest": support::DIGEST},
                           "started_at": "2026-01-01T00:00:00.000Z", "finished_at": "2026-01-01T00:00:01.000Z",
                           "result": "passed", "digest": support::DIGEST, "output": blob(), "approval_id": "fixture-check"}),
                );
                generator.push(
                    "review.result",
                    OPERATOR,
                    json!({"node": node(index), "delivery": delivery, "causes": [observed]}),
                    json!({"verdict": if index % 20 == 0 { "accept" } else { "send_back" }, "reason": "Checked"}),
                );
            }
            generator.push(
                "work.progress",
                &writer,
                json!({"node": node(index), "attempt": started}),
                json!({"agent": agent, "evidence": [], "status": "done"}),
            );
            // Every 30th feature is restarted once and finished again.
            if index % 30 == 5 {
                let restarted = generator.push(
                    "work.progress",
                    &writer,
                    json!({"node": node(index), "causes": [started]}),
                    json!({"agent": agent, "eta": {"max_s": 600, "min_s": 120}, "evidence": [], "status": "started"}),
                );
                generator.push(
                    "work.progress",
                    &writer,
                    json!({"node": node(index), "attempt": restarted}),
                    json!({"agent": agent, "evidence": [], "status": "done"}),
                );
            }
        }
    }
    let as_of = generator.base.add_seconds(generator.tick + 600);
    (generator.events, as_of)
}
