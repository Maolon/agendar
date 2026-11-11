//! Source-bound wire decoding and validation.
//!
//! This module performs wire checks only. It never resolves a referenced
//! event, reads the filesystem, indexes, or reduces state.

use std::collections::BTreeSet;
use std::fmt;

use serde::Deserialize;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

use crate::error::{AgendarError, Result};
use crate::model::cards::{
    CardEvidence, CheckResult, Claim, Criterion, CriterionResult, DecisionCard, Demo,
    ObservationMethod, ReviewAnchor, ReviewComment, ReviewVerdict,
};
use crate::model::event::{
    Budget, DecisionAck, DecisionAnswer, DecisionAsk, DecisionChoice, DecisionWithdrawn, Delivered,
    Delivery, Event, EventPayload, EvidenceObserved, MAX_NEED_ID_CHARS, MAX_SNOOZE_MS, NeedSnoozed,
    PlanDropped, PlanFeature, PolicyCells, PolicyEvaluation, PolicySet, Preset, ProjectDeclared,
    Pulse, Refs, Reply, ReportPublished, ReviewResult, RuleSet, SessionContinued, SessionLost,
    SourcePath, WorkProgress, WorkStatus,
};
use crate::model::ids::{BlobExt, BlobRef, EventId, Mode, WriterRole};

const MAX_LINE_BYTES: usize = 65_536;
const MAX_SEQ: u64 = 9_007_199_254_740_991;
const MAX_JSON_DEPTH: usize = 32;
const ENVELOPE_KEYS: [&str; 9] = [
    "v", "id", "seq", "writer", "ts", "project", "refs", "kind", "body",
];

/// Option icons agents may use.
pub const ICON_NAMES: [&str; 44] = [
    "git-branch",
    "package",
    "circle-dashed",
    "alert-triangle",
    "eye",
    "message-square",
    "help-circle",
    "undo-2",
    "hourglass",
    "lock",
    "clock",
    "timer",
    "gauge",
    "shield",
    "bot",
    "user",
    "check",
    "x",
    "flag",
    "badge-check",
    "drama",
    "arrow-right",
    "refresh-cw",
    "pause-circle",
    "eye-off",
    "file-text",
    "file-video",
    "search",
    "chevron-right",
    "corner-down-left",
    "inbox",
    "target",
    "star",
    "link",
    "radar",
    "radio",
    "server",
    "cloud",
    "terminal",
    "hand",
    "columns-2",
    "box",
    "circle-dot",
    "library",
];

/// Project glyphs agents may declare.
pub const PROJECT_GLYPHS: [&str; 10] = [
    "radar",
    "radio",
    "box",
    "columns-2",
    "circle-dot",
    "library",
    "file-text",
    "server",
    "cloud",
    "terminal",
];

/// Text limits of agent-written fields, in Unicode scalars after trimming.
/// `agendar schema` prints these, so agents see the same numbers the
/// validator enforces.
/// Text limits are soft: text up to a quarter longer is still accepted (see
/// [`grace`]), and the schema shows the soft value agents should aim for.
pub mod limits {
    pub const CARD_WHY_NOW: usize = 140;
    pub const CARD_QUESTION: usize = 110;
    pub const OPTION_LABEL: usize = 50;
    pub const OPTION_CONSEQUENCE: usize = 130;
    pub const OPTION_COST: usize = 100;
    pub const RECOMMENDED_BECAUSE: usize = 130;
    pub const CRITERION_TEXT: usize = 140;
    pub const FACT_TEXT: usize = 140;
    pub const FEATURE_TITLE: usize = 110;
    pub const FEATURE_SLICE: usize = 100;
    pub const FEATURE_MILESTONE: usize = 100;
    pub const FEATURE_DEPENDS_ON: usize = 64;
    pub const PROJECT_TITLE: usize = 100;
    pub const PROJECT_BRIEF: usize = 220;
    pub const PROJECT_GOAL: usize = 220;
    pub const NOTE: usize = 240;
    pub const PULSE_MESSAGE: usize = 240;
    pub const REPORT_TITLE: usize = 110;
    pub const REPORT_SUMMARY: usize = 800;
}

/// The hard limit behind a soft text limit: a quarter more, so text a little
/// over the aim is not rejected.
pub const fn grace(max: usize) -> usize {
    max + max / 4
}

fn validation_at(
    field: impl Into<String>,
    detail: impl Into<String>,
    message: impl Into<String>,
) -> AgendarError {
    AgendarError::validation_at(field, detail, message)
}

/// Parse one complete LF/CRLF-terminated event line against its source path.
pub fn parse_event(line: &[u8], source: &SourcePath) -> Result<Event> {
    let text = complete_line(line)?;
    let StrictValue(root) = serde_json::from_str::<StrictValue>(text).map_err(|error| {
        validation_at("/", "invalid_json", format!("Invalid event JSON: {error}"))
    })?;
    if json_depth(&root) > MAX_JSON_DEPTH {
        return Err(validation_at(
            "/",
            "json_nesting_limit",
            format!("Event JSON nesting must be at most {MAX_JSON_DEPTH}."),
        ));
    }
    let Some(object) = root.as_object() else {
        return Err(validation_at(
            "/",
            "invalid_envelope",
            "Event envelope must be a JSON object.",
        ));
    };
    validate_envelope_keys(object)?;
    validate_wire_shape(&root, "")?;

    let version = map_u64(object, "v")?;
    if version != 1 {
        return Err(validation_at(
            "/v",
            "unsupported_version",
            "Event version must be exactly 1.",
        ));
    }
    let id = EventId::parse(map_str(object, "id")?).map_err(|error| error.with_field("/id"))?;
    let seq = map_u64(object, "seq")?;
    if !(1..=MAX_SEQ).contains(&seq) {
        return Err(validation_at(
            "/seq",
            "sequence_range",
            format!("Event seq must be 1..={MAX_SEQ}; got {seq}."),
        ));
    }
    let writer = crate::model::ids::WriterId::parse(map_str(object, "writer")?)
        .map_err(|error| error.with_field("/writer"))?;
    let ts = crate::model::ids::UtcTime::parse(map_str(object, "ts")?)
        .map_err(|error| error.with_field("/ts"))?;
    let project = crate::model::ids::ProjectSlug::parse(map_str(object, "project")?)
        .map_err(|error| error.with_field("/project"))?;
    let refs_value = object
        .get("refs")
        .ok_or_else(|| validation_at("/refs", "missing_field", "Event refs must be present."))?;
    let (refs, presence) = decode_refs(refs_value)?;
    let kind = map_str(object, "kind")?;
    let body = object
        .get("body")
        .ok_or_else(|| validation_at("/body", "missing_field", "Event body must be present."))?;
    let payload = decode_payload(kind, body)?;

    let event = Event {
        v: 1,
        id,
        seq,
        writer,
        ts,
        project,
        refs,
        payload,
    };
    validate_event(&event, source, &presence)?;
    Ok(event)
}

fn complete_line(line: &[u8]) -> Result<&str> {
    if line.len() > MAX_LINE_BYTES {
        return Err(validation_at(
            "/",
            "line_size",
            format!("Event line must be at most {MAX_LINE_BYTES} bytes including LF."),
        ));
    }
    let Some((&b'\n', mut body)) = line.split_last() else {
        return Err(validation_at(
            "/",
            "line_terminator",
            "Event line must end with LF or CRLF.",
        ));
    };
    if body.last() == Some(&b'\r') {
        body = &body[..body.len() - 1];
    }
    if body.contains(&b'\n') || body.contains(&b'\r') {
        return Err(validation_at(
            "/",
            "line_terminator",
            "Event line must contain exactly one complete line.",
        ));
    }
    std::str::from_utf8(body).map_err(|_| {
        validation_at(
            "/",
            "invalid_utf8",
            "Event line must be complete UTF-8 after line assembly.",
        )
    })
}

fn map_u64(object: &Map<String, Value>, key: &str) -> Result<u64> {
    object.get(key).and_then(Value::as_u64).ok_or_else(|| {
        validation_at(
            format!("/{key}"),
            "invalid_type",
            format!("Envelope field `{key}` must be an unsigned integer."),
        )
    })
}

fn map_str<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    object.get(key).and_then(Value::as_str).ok_or_else(|| {
        validation_at(
            format!("/{key}"),
            "invalid_type",
            format!("Envelope field `{key}` must be a string."),
        )
    })
}

fn validate_envelope_keys(object: &Map<String, Value>) -> Result<()> {
    for key in object.keys() {
        if !ENVELOPE_KEYS.contains(&key.as_str()) {
            return Err(validation_at(
                format!("/{key}"),
                "unknown_field",
                format!("Unknown envelope field `{key}`."),
            ));
        }
    }
    for key in ENVELOPE_KEYS {
        if !object.contains_key(key) {
            return Err(validation_at(
                format!("/{key}"),
                "missing_field",
                format!("Missing envelope field `{key}`."),
            ));
        }
    }
    Ok(())
}

/// Every JSON array the wire model defines. `*` matches one array index.
///
/// Require exact object shapes and forbid unknown
/// fields at every nesting level. Serde's derived struct decoder would otherwise
/// also accept a positional array of field values for any object-shaped slot, so
/// every array in a raw event must appear at one of these semantic positions.
/// A legitimate array added later must be listed here deliberately.
const SEMANTIC_ARRAYS: [&str; 24] = [
    "refs/causes",
    "refs/basis",
    "body/depends_on",
    "body/evidence",
    "body/criteria",
    "body/demos",
    "body/demos/*/body/check/argv",
    "body/demos/*/body/shown",
    "body/demos/*/body/omitted",
    "body/done_when",
    "body/comments",
    "body/replies",
    "body/restarted",
    "body/disclosure/proven",
    "body/disclosure/not_proven_yet",
    "body/disclosure/faked_for_now",
    "body/card/options",
    "body/card/already_checked",
    "body/card/done_when",
    "body/card/evidence/items",
    "body/card/evidence/rows",
    "body/card/evidence/rows/*",
    "body/policy/heuristic_hits",
    "body/policy/budget/reasons",
];

fn validate_wire_shape(value: &Value, path: &str) -> Result<()> {
    match value {
        Value::Array(items) => {
            if !SEMANTIC_ARRAYS
                .iter()
                .any(|pattern| path_matches(pattern, path))
            {
                return Err(validation_at(
                    format!("/{path}"),
                    "object_shape_required",
                    format!(
                        "Object-shaped wire value `/{path}` must be a JSON object; positional arrays are not accepted."
                    ),
                ));
            }
            for (index, item) in items.iter().enumerate() {
                validate_wire_shape(item, &format!("{path}/{index}"))?;
            }
        }
        Value::Object(fields) => {
            for (key, item) in fields {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}/{key}")
                };
                validate_wire_shape(item, &child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn path_matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    pattern.len() == path.len()
        && pattern
            .iter()
            .zip(path)
            .all(|(expected, actual)| *expected == "*" || *expected == actual)
}

#[derive(Debug, Clone, Copy, Default)]
struct RefPresence {
    basis: bool,
}
fn decode_refs(value: &Value) -> Result<(Refs, RefPresence)> {
    let Some(object) = value.as_object() else {
        return Err(validation_at(
            "/refs",
            "invalid_type",
            "Event refs must be an object.",
        ));
    };
    const REF_KEYS: [&str; 9] = [
        "node",
        "ask",
        "answer",
        "delivery",
        "receipt",
        "attempt",
        "supersedes",
        "causes",
        "basis",
    ];
    for key in object.keys() {
        if !REF_KEYS.contains(&key.as_str()) {
            return Err(validation_at(
                format!("/refs/{key}"),
                "unknown_field",
                format!("Unknown refs field `{key}`."),
            ));
        }
    }
    let presence = RefPresence {
        basis: object.contains_key("basis"),
    };
    let refs = serde_json::from_value::<Refs>(value.clone()).map_err(|error| {
        validation_at("/refs", "invalid_refs", format!("Invalid refs: {error}"))
    })?;
    Ok((refs, presence))
}

fn body_error(kind: &str, error: &serde_json::Error) -> AgendarError {
    validation_at(
        "/body",
        "invalid_body",
        format!("Invalid {kind} body: {error}"),
    )
}

fn decode_payload(kind: &str, body: &Value) -> Result<EventPayload> {
    let Some(object) = body.as_object() else {
        return Err(validation_at(
            "/body",
            "invalid_type",
            "Event body must be an object.",
        ));
    };
    if matches!(kind, "policy.set" | "rule.set")
        && object.contains_key("value")
        && object.contains_key("revision")
    {
        return Err(validation_at(
            "/body",
            "projection_wrapper_as_body",
            "Projection wrappers (value + revision) are never event bodies.",
        ));
    }
    macro_rules! decode_body {
        ($variant:path, $ty:ty) => {
            serde_json::from_value::<$ty>(body.clone())
                .map($variant)
                .map_err(|error| body_error(kind, &error))
        };
    }
    match kind {
        "project.declared" => decode_body!(EventPayload::ProjectDeclared, ProjectDeclared),
        "plan.feature" => decode_body!(EventPayload::PlanFeature, PlanFeature),
        "plan.dropped" => decode_body!(EventPayload::PlanDropped, PlanDropped),
        "need.snoozed" => decode_body!(EventPayload::NeedSnoozed, NeedSnoozed),
        "work.progress" => decode_body!(EventPayload::WorkProgress, WorkProgress),
        "pulse" => decode_body!(EventPayload::Pulse, Pulse),
        "decision.ask" => decode_body!(EventPayload::DecisionAsk, DecisionAsk),
        "decision.assumed" => decode_body!(EventPayload::DecisionAssumed, DecisionChoice),
        "decision.auto" => decode_body!(EventPayload::DecisionAuto, DecisionChoice),
        "report.published" => decode_body!(EventPayload::ReportPublished, ReportPublished),
        "decision.withdrawn" => {
            decode_body!(EventPayload::DecisionWithdrawn, DecisionWithdrawn)
        }
        "decision.answer" => decode_body!(EventPayload::DecisionAnswer, DecisionAnswer),
        "decision.ack" => decode_body!(EventPayload::DecisionAck, DecisionAck),
        "delivery" => decode_body!(EventPayload::Delivery, Delivery),
        "evidence.observed" => {
            decode_body!(EventPayload::EvidenceObserved, EvidenceObserved)
        }
        "review.result" => decode_body!(EventPayload::ReviewResult, ReviewResult),
        "rule.set" => decode_body!(EventPayload::RuleSet, RuleSet),
        "policy.set" => decode_body!(EventPayload::PolicySet, PolicySet),
        "session.lost" => decode_body!(EventPayload::SessionLost, SessionLost),
        "session.continued" => decode_body!(EventPayload::SessionContinued, SessionContinued),
        "delivered" => decode_body!(EventPayload::Delivered, Delivered),
        other => Err(validation_at(
            "/kind",
            "unknown_kind",
            format!("Unknown event kind `{other}`."),
        )),
    }
}

fn validate_event(event: &Event, source: &SourcePath, presence: &RefPresence) -> Result<()> {
    validate_source(event, source)?;
    if event.v != 1 {
        return Err(validation_at(
            "/v",
            "unsupported_version",
            "Event version must be exactly 1.",
        ));
    }
    if !(1..=MAX_SEQ).contains(&event.seq) {
        return Err(validation_at(
            "/seq",
            "sequence_range",
            "Event seq is outside the JS-safe range.",
        ));
    }
    validate_role_and_agent(event)?;
    validate_self_references(event)?;
    validate_duplicate_refs(event)?;
    validate_ref_matrix(event, presence)?;
    validate_body(event)?;
    validate_body_references(event)?;
    Ok(())
}

fn validate_source(event: &Event, source: &SourcePath) -> Result<()> {
    let parsed = SourcePath::from_relative_path(&source.relative_path).map_err(|_| {
        validation_at(
            "/source",
            "inconsistent_source_path",
            "Source path fields do not match their relative path.",
        )
    })?;
    if parsed.project != source.project
        || parsed.writer != source.writer
        || parsed.segment != source.segment
    {
        return Err(validation_at(
            "/source",
            "inconsistent_source_path",
            "SourcePath fields must equal the parsed relative path.",
        ));
    }
    if event.project != parsed.project {
        return Err(validation_at(
            "/project",
            "source_project_mismatch",
            "Envelope project does not match the source path.",
        ));
    }
    if event.writer != parsed.writer {
        return Err(validation_at(
            "/writer",
            "source_writer_mismatch",
            "Envelope writer does not match the source path.",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoleRule {
    HumanOrAgent,
    HumanOnly,
    StewardOnly,
    AgentOnly,
}

fn role_rule(kind: &str) -> RoleRule {
    match kind {
        "project.declared" | "plan.feature" => RoleRule::HumanOrAgent,
        "decision.answer" | "review.result" | "rule.set" | "policy.set" | "plan.dropped"
        | "need.snoozed" => RoleRule::HumanOnly,
        "evidence.observed" | "session.lost" | "session.continued" | "delivered" => {
            RoleRule::StewardOnly
        }
        _ => RoleRule::AgentOnly,
    }
}

fn validate_role_and_agent(event: &Event) -> Result<()> {
    let role = event.writer.role();
    let rule = role_rule(event.kind());
    let kind = event.kind();
    match rule {
        RoleRule::HumanOrAgent => match role {
            WriterRole::Operator => {
                if event.payload.agent().is_some() {
                    return Err(validation_at(
                        "/agent",
                        "agent_forbidden",
                        format!("{kind} by an operator writer must omit agent."),
                    ));
                }
            }
            WriterRole::Agent(session) => {
                let Some(agent) = event.payload.agent() else {
                    return Err(validation_at(
                        "/agent",
                        "agent_required",
                        format!("{kind} by an agent writer requires agent."),
                    ));
                };
                if &agent.session != session {
                    return Err(agent_session_mismatch());
                }
            }
            WriterRole::Steward => {
                return Err(role_forbidden(kind, "a human or agent"));
            }
        },
        RoleRule::HumanOnly => {
            if !event.writer.is_operator() {
                return Err(role_forbidden(kind, "an operator"));
            }
        }
        RoleRule::StewardOnly => {
            if !event.writer.is_steward() {
                return Err(role_forbidden(kind, "a steward"));
            }
        }
        RoleRule::AgentOnly => {
            let WriterRole::Agent(session) = role else {
                return Err(role_forbidden(kind, "an agent"));
            };
            let Some(agent) = event.payload.agent() else {
                return Err(validation_at(
                    "/agent",
                    "agent_required",
                    format!("{kind} requires agent."),
                ));
            };
            if &agent.session != session {
                return Err(agent_session_mismatch());
            }
        }
    }
    Ok(())
}

fn role_forbidden(kind: &str, expected: &str) -> AgendarError {
    validation_at(
        "/writer",
        "writer_role_forbidden",
        format!("{kind} requires {expected} writer."),
    )
}

fn agent_session_mismatch() -> AgendarError {
    validation_at(
        "/agent/session",
        "agent_session_mismatch",
        "Agent session must equal the writer session.",
    )
}

fn validate_self_references(event: &Event) -> Result<()> {
    let refs = &event.refs;
    for (field, id) in scalar_refs(refs) {
        if id == &event.id {
            return Err(validation_at(
                field,
                "self_reference",
                "An event must not reference itself.",
            ));
        }
    }
    for (index, id) in refs.causes.iter().enumerate() {
        if id == &event.id {
            return Err(validation_at(
                format!("/refs/causes/{index}"),
                "self_reference",
                "An event must not reference itself.",
            ));
        }
    }
    for (index, id) in refs.basis.iter().enumerate() {
        if id == &event.id {
            return Err(validation_at(
                format!("/refs/basis/{index}"),
                "self_reference",
                "An event must not reference itself.",
            ));
        }
    }
    Ok(())
}

fn validate_duplicate_refs(event: &Event) -> Result<()> {
    let refs = &event.refs;
    if refs.causes.len() > 128 {
        return Err(validation_at(
            "/refs/causes",
            "refs_limit",
            "refs.causes must contain at most 128 IDs.",
        ));
    }
    if refs.basis.len() > 128 {
        return Err(validation_at(
            "/refs/basis",
            "refs_limit",
            "refs.basis must contain at most 128 IDs.",
        ));
    }
    let mut seen = BTreeSet::new();
    for (index, id) in refs.causes.iter().enumerate() {
        if !seen.insert(id.as_str()) {
            return Err(validation_at(
                format!("/refs/causes/{index}"),
                "duplicate_ref",
                "Duplicate IDs are not allowed in refs.causes.",
            ));
        }
    }
    seen.clear();
    for (index, id) in refs.basis.iter().enumerate() {
        if !seen.insert(id.as_str()) {
            return Err(validation_at(
                format!("/refs/basis/{index}"),
                "duplicate_ref",
                "Duplicate IDs are not allowed in refs.basis.",
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefName {
    Node,
    Ask,
    Answer,
    Delivery,
    Receipt,
    Attempt,
    Supersedes,
}

const ALL_SCALAR_REFS: [RefName; 7] = [
    RefName::Node,
    RefName::Ask,
    RefName::Answer,
    RefName::Delivery,
    RefName::Receipt,
    RefName::Attempt,
    RefName::Supersedes,
];

#[derive(Debug, Clone, Copy)]
struct RefRule {
    required: &'static [RefName],
    allowed: &'static [RefName],
    basis: bool,
}

const NONE_REFS: &[RefName] = &[];
const NODE_REFS: &[RefName] = &[RefName::Node];
const NODE_ATTEMPT_REFS: &[RefName] = &[RefName::Node, RefName::Attempt];
const NODE_ASK_REFS: &[RefName] = &[RefName::Node, RefName::Ask];
const NODE_ASK_ANSWER_REFS: &[RefName] = &[RefName::Node, RefName::Ask, RefName::Answer];
const NODE_DELIVERY_REFS: &[RefName] = &[RefName::Node, RefName::Delivery];
const NODE_SUPERSEDES_REFS: &[RefName] = &[RefName::Node, RefName::Supersedes];
const NODE_ASK_ATTEMPT_REFS: &[RefName] = &[RefName::Node, RefName::Ask, RefName::Attempt];
const NODE_ATTEMPT_RECEIPT_SUPERSEDES_REFS: &[RefName] = &[
    RefName::Node,
    RefName::Attempt,
    RefName::Receipt,
    RefName::Supersedes,
];

fn ref_name_key(name: RefName) -> &'static str {
    match name {
        RefName::Node => "node",
        RefName::Ask => "ask",
        RefName::Answer => "answer",
        RefName::Delivery => "delivery",
        RefName::Receipt => "receipt",
        RefName::Attempt => "attempt",
        RefName::Supersedes => "supersedes",
    }
}

fn ref_present(name: RefName, refs: &Refs) -> bool {
    match name {
        RefName::Node => refs.node.is_some(),
        RefName::Ask => refs.ask.is_some(),
        RefName::Answer => refs.answer.is_some(),
        RefName::Delivery => refs.delivery.is_some(),
        RefName::Receipt => refs.receipt.is_some(),
        RefName::Attempt => refs.attempt.is_some(),
        RefName::Supersedes => refs.supersedes.is_some(),
    }
}

fn ref_rule(event: &Event) -> Result<RefRule> {
    let rule = match &event.payload {
        EventPayload::ProjectDeclared(_)
        | EventPayload::RuleSet(_)
        | EventPayload::PolicySet(_)
        | EventPayload::NeedSnoozed(_) => RefRule {
            required: NONE_REFS,
            allowed: NONE_REFS,
            basis: false,
        },
        EventPayload::PlanFeature(_) | EventPayload::PlanDropped(_) => RefRule {
            required: NODE_REFS,
            allowed: NODE_REFS,
            basis: false,
        },
        EventPayload::WorkProgress(body) => {
            if body.status == WorkStatus::Started {
                RefRule {
                    required: NODE_REFS,
                    allowed: NODE_REFS,
                    basis: true,
                }
            } else if body.status == WorkStatus::Dropped {
                // A never-started step is dropped without an attempt.
                RefRule {
                    required: NODE_REFS,
                    allowed: NODE_ATTEMPT_REFS,
                    basis: true,
                }
            } else {
                RefRule {
                    required: NODE_ATTEMPT_REFS,
                    allowed: NODE_ATTEMPT_REFS,
                    basis: true,
                }
            }
        }
        EventPayload::Pulse(_) => match (&event.refs.node, &event.refs.attempt) {
            (Some(_), Some(_)) => RefRule {
                required: NODE_ATTEMPT_REFS,
                allowed: NODE_ATTEMPT_REFS,
                basis: false,
            },
            (None, None) => RefRule {
                required: NONE_REFS,
                allowed: NONE_REFS,
                basis: false,
            },
            _ => {
                return Err(validation_at(
                    "/refs/node",
                    "pulse_node_attempt_pair",
                    "Pulse node and attempt must both be present or both be absent.",
                ));
            }
        },
        EventPayload::DecisionAsk(_) => RefRule {
            required: NODE_REFS,
            allowed: NODE_SUPERSEDES_REFS,
            basis: false,
        },
        EventPayload::DecisionAssumed(_) | EventPayload::DecisionAuto(_) => RefRule {
            required: NODE_ASK_REFS,
            allowed: NODE_ASK_ATTEMPT_REFS,
            basis: false,
        },
        EventPayload::ReportPublished(_) => RefRule {
            required: NONE_REFS,
            allowed: NODE_REFS,
            basis: false,
        },
        EventPayload::DecisionWithdrawn(_) | EventPayload::DecisionAnswer(_) => RefRule {
            required: NODE_ASK_REFS,
            allowed: NODE_ASK_REFS,
            basis: false,
        },
        EventPayload::DecisionAck(_) => RefRule {
            required: NODE_ASK_ANSWER_REFS,
            allowed: NODE_ASK_ANSWER_REFS,
            basis: false,
        },
        // The receipt is optional: finished work no decision asked for is
        // delivered without one.
        EventPayload::Delivery(_) => RefRule {
            required: NODE_ATTEMPT_REFS,
            allowed: NODE_ATTEMPT_RECEIPT_SUPERSEDES_REFS,
            basis: false,
        },
        EventPayload::EvidenceObserved(_) | EventPayload::ReviewResult(_) => RefRule {
            required: NODE_DELIVERY_REFS,
            allowed: NODE_DELIVERY_REFS,
            basis: false,
        },
        EventPayload::SessionLost(_) => RefRule {
            required: NONE_REFS,
            allowed: NODE_REFS,
            basis: false,
        },
        EventPayload::SessionContinued(_) => RefRule {
            required: NONE_REFS,
            allowed: NONE_REFS,
            basis: false,
        },
        EventPayload::Delivered(_) => {
            if event.refs.delivery.is_some() {
                if event.refs.causes.is_empty() {
                    return Err(validation_at(
                        "/refs/causes",
                        "delivered_review_requires_causes",
                        "A delivered review packet requires a nonempty causes list.",
                    ));
                }
                RefRule {
                    required: NODE_DELIVERY_REFS,
                    allowed: NODE_DELIVERY_REFS,
                    basis: false,
                }
            } else {
                RefRule {
                    required: NODE_ASK_ANSWER_REFS,
                    allowed: NODE_ASK_ANSWER_REFS,
                    basis: false,
                }
            }
        }
    };
    Ok(rule)
}

fn validate_ref_matrix(event: &Event, presence: &RefPresence) -> Result<()> {
    let rule = ref_rule(event)?;
    let refs = &event.refs;
    for name in ALL_SCALAR_REFS {
        if ref_present(name, refs) && !rule.allowed.contains(&name) {
            return Err(validation_at(
                format!("/refs/{}", ref_name_key(name)),
                "forbidden_ref",
                format!(
                    "{} ref is not allowed for {}.",
                    ref_name_key(name),
                    event.kind()
                ),
            ));
        }
    }
    for name in rule.required {
        if !ref_present(*name, refs) {
            return Err(validation_at(
                format!("/refs/{}", ref_name_key(*name)),
                "required_ref_missing",
                format!(
                    "{} ref is required for {}.",
                    ref_name_key(*name),
                    event.kind()
                ),
            ));
        }
    }
    if presence.basis && !rule.basis {
        return Err(validation_at(
            "/refs/basis",
            "forbidden_ref",
            format!("refs.basis is not allowed for {}.", event.kind()),
        ));
    }
    Ok(())
}

fn scalar_refs(refs: &Refs) -> Vec<(&'static str, &EventId)> {
    let mut values = Vec::new();
    if let Some(id) = &refs.ask {
        values.push(("/refs/ask", id));
    }
    if let Some(id) = &refs.answer {
        values.push(("/refs/answer", id));
    }
    if let Some(id) = &refs.delivery {
        values.push(("/refs/delivery", id));
    }
    if let Some(id) = &refs.receipt {
        values.push(("/refs/receipt", id));
    }
    if let Some(id) = &refs.attempt {
        values.push(("/refs/attempt", id));
    }
    if let Some(id) = &refs.supersedes {
        values.push(("/refs/supersedes", id));
    }
    values
}

fn validate_body(event: &Event) -> Result<()> {
    match &event.payload {
        EventPayload::ProjectDeclared(body) => validate_project_declared(body),
        EventPayload::PlanFeature(body) => validate_plan_feature(body, event),
        EventPayload::PlanDropped(body) => match &body.note {
            Some(note) => validate_note(note),
            None => Ok(()),
        },
        EventPayload::NeedSnoozed(body) => validate_need_snoozed(body, event),
        EventPayload::WorkProgress(body) => validate_work_progress(body),
        EventPayload::Pulse(body) => validate_pulse(body),
        EventPayload::DecisionAsk(body) => validate_decision_ask(body),
        EventPayload::DecisionAssumed(body) => validate_decision_choice(body, false),
        EventPayload::DecisionAuto(body) => validate_decision_choice(body, true),
        EventPayload::DecisionWithdrawn(body) => validate_decision_withdrawn(body),
        EventPayload::ReportPublished(body) => validate_report_published(body),
        EventPayload::DecisionAnswer(body) => validate_decision_answer(body),
        EventPayload::DecisionAck(body) => validate_decision_ack(body),
        EventPayload::Delivery(body) => validate_delivery(body, event),
        EventPayload::EvidenceObserved(body) => validate_evidence_observed(body, event),
        EventPayload::ReviewResult(body) => validate_review_result(body),
        EventPayload::RuleSet(body) => validate_rule_set(body, "/"),
        EventPayload::PolicySet(body) => validate_policy_set(body),
        EventPayload::SessionLost(body) => validate_session_lost(body),
        EventPayload::SessionContinued(body) => validate_session_continued(body),
        EventPayload::Delivered(body) => validate_delivered(body),
    }
}

fn validate_project_declared(body: &ProjectDeclared) -> Result<()> {
    if body.title.is_none()
        && body.brief.is_none()
        && body.goal.is_none()
        && body.glyph.is_none()
        && body.archived.is_none()
        && body.icon.is_none()
    {
        return Err(validation_at(
            "/body",
            "empty_patch",
            "project.declared must contain at least one patch field.",
        ));
    }
    if let Some(icon) = &body.icon {
        icon.validate("/icon")?;
    }
    if let Some(icon) = &body.icon
        && !ICON_EXTS.contains(&icon.ext)
    {
        return Err(validation_at(
            "/icon",
            "icon_not_image",
            "A project icon must be a png, jpg, webp or svg image.",
        ));
    }
    if let Some(title) = &body.title {
        validate_text("/title", title, 1, limits::PROJECT_TITLE, false)?;
    }
    if let Some(brief) = &body.brief {
        validate_text("/brief", brief, 1, limits::PROJECT_BRIEF, false)?;
    }
    if let Some(goal) = &body.goal {
        validate_text("/goal", goal, 1, limits::PROJECT_GOAL, false)?;
    }
    if let Some(glyph) = &body.glyph
        && !PROJECT_GLYPHS.contains(&glyph.as_str())
    {
        return Err(validation_at(
            "/glyph",
            "unknown_glyph",
            format!("glyph {glyph:?} is not a project glyph."),
        )
        .with_hint(format!("Use one of: {}.", PROJECT_GLYPHS.join(", "))));
    }
    // Agents may archive a project they finished (automation needs it); a
    // human's archived value still outranks theirs in the fold.
    Ok(())
}

fn validate_plan_feature(body: &PlanFeature, event: &Event) -> Result<()> {
    if body.title.is_none()
        && body.depends_on.is_none()
        && body.slice.is_none()
        && body.demo_kind.is_none()
        && body.milestone.is_none()
        && body.deadline.is_none()
        && body.priority.is_none()
        && body.paused.is_none()
        && body.handoff.is_none()
    {
        return Err(validation_at(
            "/body",
            "empty_patch",
            "plan.feature must contain at least one patch field.",
        ));
    }
    if let Some(title) = &body.title {
        validate_text("/title", title, 1, limits::FEATURE_TITLE, false)?;
    }
    if let Some(slice) = &body.slice {
        validate_text("/slice", slice, 1, limits::FEATURE_SLICE, false)?;
    }
    if let Some(milestone) = &body.milestone {
        validate_text("/milestone", milestone, 1, limits::FEATURE_MILESTONE, false)?;
    }
    if let Some(depends_on) = &body.depends_on {
        if depends_on.len() > limits::FEATURE_DEPENDS_ON {
            return Err(validation_at(
                "/depends_on",
                "depends_on_limit",
                "depends_on must contain at most 64 node paths.",
            ));
        }
        let mut seen = BTreeSet::new();
        for (index, node) in depends_on.iter().enumerate() {
            if !seen.insert(node.as_str()) {
                return Err(validation_at(
                    format!("/depends_on/{index}"),
                    "duplicate_node",
                    "depends_on must not contain duplicate node paths.",
                ));
            }
        }
    }
    if let Some(priority) = body.priority
        && priority > 3
    {
        return Err(validation_at(
            "/priority",
            "priority_range",
            "priority must be 0..=3.",
        ));
    }
    if body.paused.is_some() && !event.writer.is_operator() {
        return Err(validation_at(
            "/paused",
            "human_only_field",
            "paused is human-only from the first write.",
        ));
    }
    if body.handoff.is_some() && !event.writer.is_operator() {
        return Err(validation_at(
            "/handoff",
            "human_only_field",
            "handoff is human-only from the first write.",
        ));
    }
    Ok(())
}

fn validate_work_progress(body: &WorkProgress) -> Result<()> {
    match body.status {
        WorkStatus::Started => {
            if body.eta.is_none() {
                return Err(validation_at(
                    "/eta",
                    "started_requires_eta",
                    "work.progress started requires eta.",
                ));
            }
        }
        WorkStatus::Done | WorkStatus::Verified | WorkStatus::Dropped => {
            if body.eta.is_some() {
                return Err(validation_at(
                    "/eta",
                    "eta_only_started",
                    "eta is only allowed on work.progress started.",
                ));
            }
        }
    }
    if let Some(eta) = &body.eta {
        eta.validate("/eta")?;
    }
    if body.evidence.len() > 3 {
        return Err(validation_at(
            "/evidence",
            "evidence_limit",
            "work.progress evidence must contain at most 3 results.",
        ));
    }
    let mut seen = BTreeSet::new();
    for (index, result) in body.evidence.iter().enumerate() {
        if !seen.insert(result.criterion.as_str()) {
            return Err(validation_at(
                format!("/evidence/{index}/criterion"),
                "duplicate_criterion",
                "work.progress evidence must not repeat a criterion.",
            ));
        }
        validate_criterion_result(
            &format!("/evidence/{index}"),
            result,
            CriterionContext::Progress,
        )?;
    }
    if let Some(note) = &body.note {
        validate_text("/note", note, 1, limits::NOTE, false)?;
    }
    if body.status == WorkStatus::Verified
        && !body.evidence.iter().any(|result| {
            result.status == CheckResult::Passed
                && (result.evidence.is_some() || result.observation.is_some())
        })
    {
        return Err(validation_at(
            "/evidence",
            "verified_requires_passed_evidence",
            "work.progress verified requires at least one passed result with a blob or observation.",
        ));
    }
    Ok(())
}

fn validate_pulse(body: &Pulse) -> Result<()> {
    validate_text("/message", &body.message, 1, limits::PULSE_MESSAGE, false)?;
    if let Some(eta) = &body.eta {
        eta.validate("/eta")?;
    }
    Ok(())
}

fn validate_decision_ask(body: &DecisionAsk) -> Result<()> {
    validate_card(&body.card)?;
    validate_policy_evaluation(&body.policy, "/policy")
}

fn validate_decision_choice(body: &DecisionChoice, is_auto: bool) -> Result<()> {
    if is_auto && body.reason == crate::model::event::ChoiceReason::Timeout {
        return Err(validation_at(
            "/reason",
            "auto_cannot_timeout",
            "decision.auto cannot use reason timeout.",
        ));
    }
    validate_policy_evaluation(&body.policy, "/policy")
}

fn validate_report_published(body: &ReportPublished) -> Result<()> {
    validate_text("/title", &body.title, 1, limits::REPORT_TITLE, false)?;
    validate_text("/summary", &body.summary, 1, limits::REPORT_SUMMARY, true)?;
    if !matches!(body.report.ext, BlobExt::Html | BlobExt::Md | BlobExt::Txt) {
        return Err(validation_at(
            "/report",
            "report_not_document",
            "A report is an html, md or txt document.",
        ));
    }
    Ok(())
}

fn validate_decision_withdrawn(body: &DecisionWithdrawn) -> Result<()> {
    validate_text("/reason", &body.reason, 1, 160, true)
}

fn validate_decision_answer(body: &DecisionAnswer) -> Result<()> {
    if body.done_when.is_empty() || body.done_when.len() > 3 {
        return Err(validation_at(
            "/done_when",
            "done_when_count",
            "done_when must contain 1..=3 criteria.",
        ));
    }
    let mut seen = BTreeSet::new();
    for (index, criterion) in body.done_when.iter().enumerate() {
        if !seen.insert(criterion.id.as_str()) {
            return Err(validation_at(
                format!("/done_when/{index}/id"),
                "duplicate_criterion",
                "done_when must not repeat a criterion ID.",
            ));
        }
        validate_criterion(&format!("/done_when/{index}"), criterion)?;
    }
    if let Some(eta) = &body.expected_eta {
        eta.validate("/expected_eta")?;
    }
    if let Some(note) = &body.note {
        validate_text("/note", note, 1, 500, false)?;
    }
    let own = body.option.as_str() == crate::model::cards::OWN_OPTION;
    match (&body.own_answer, own) {
        (Some(text), true) => validate_text(
            "/own_answer",
            text,
            1,
            crate::model::cards::OWN_ANSWER_MAX,
            true,
        )?,
        (None, true) => {
            return Err(validation_at(
                "/own_answer",
                "own_answer_missing",
                "An answer in your own words needs its text.",
            ));
        }
        (Some(_), false) => {
            return Err(validation_at(
                "/own_answer",
                "own_answer_with_option",
                "own_answer goes only with option `own`.",
            ));
        }
        (None, false) => {}
    }
    Ok(())
}

fn validate_decision_ack(body: &DecisionAck) -> Result<()> {
    if body.restarted.len() > 128 {
        return Err(validation_at(
            "/restarted",
            "restarted_limit",
            "restarted must contain at most 128 node paths.",
        ));
    }
    let mut seen = BTreeSet::new();
    for (index, node) in body.restarted.iter().enumerate() {
        if !seen.insert(node.as_str()) {
            return Err(validation_at(
                format!("/restarted/{index}"),
                "duplicate_node",
                "restarted must not contain duplicate node paths.",
            ));
        }
    }
    Ok(())
}

fn validate_delivery(body: &Delivery, event: &Event) -> Result<()> {
    validate_text("/name", &body.name, 1, 80, false)?;
    // Criteria are a receipt's done_when; without a receipt the disclosure
    // carries the claims.
    if event.refs.receipt.is_none() && !body.criteria.is_empty() {
        return Err(validation_at(
            "/criteria",
            "delivery_criteria_without_receipt",
            "A delivery without a receipt must list no criteria.",
        ));
    }
    if body.demos.is_empty() || body.demos.len() > 3 {
        return Err(validation_at(
            "/demos",
            "demo_count",
            "demos must contain 1..=3 entries.",
        ));
    }
    for (index, demo) in body.demos.iter().enumerate() {
        validate_demo(&format!("/demos/{index}"), demo)?;
    }
    let mut seen = BTreeSet::new();
    for (index, result) in body.criteria.iter().enumerate() {
        if !seen.insert(result.criterion.as_str()) {
            return Err(validation_at(
                format!("/criteria/{index}/criterion"),
                "duplicate_criterion",
                "delivery criteria must not repeat a criterion.",
            ));
        }
        validate_criterion_result(
            &format!("/criteria/{index}"),
            result,
            CriterionContext::Delivery,
        )?;
    }
    validate_disclosure(&body.disclosure)?;
    validate_replies(&body.replies, event)
}

/// Replies answer the send-back of the delivery this one supersedes: one per
/// target (the reason, or comment n counted from 1).
fn validate_replies(replies: &[Reply], event: &Event) -> Result<()> {
    if replies.is_empty() {
        return Ok(());
    }
    if event.refs.supersedes.is_none() {
        return Err(validation_at(
            "/replies",
            "delivery_replies_without_supersedes",
            "replies are only allowed on a delivery that supersedes another.",
        ));
    }
    if replies.len() > MAX_REVIEW_COMMENTS + 1 {
        return Err(validation_at(
            "/replies",
            "too_many_replies",
            format!(
                "A delivery carries at most {} replies: one to the reason and one per comment.",
                MAX_REVIEW_COMMENTS + 1
            ),
        ));
    }
    let mut targets = BTreeSet::new();
    for (index, reply) in replies.iter().enumerate() {
        if reply.comment == Some(0) {
            return Err(validation_at(
                format!("/replies/{index}/comment"),
                "reply_comment_out_of_range",
                "comment numbers count from 1.",
            ));
        }
        if !targets.insert(reply.comment) {
            return Err(validation_at(
                format!("/replies/{index}"),
                "duplicate_reply",
                "At most one reply per comment, and one to the reason.",
            ));
        }
        validate_text(
            &format!("/replies/{index}/text"),
            &reply.text,
            1,
            300,
            false,
        )?;
    }
    Ok(())
}

fn validate_evidence_observed(body: &EvidenceObserved, event: &Event) -> Result<()> {
    if body.finished_at < body.started_at {
        return Err(validation_at(
            "/finished_at",
            "finished_before_started",
            "finished_at must be >= started_at.",
        ));
    }
    if body.digest != body.output.sha256 {
        return Err(validation_at(
            "/digest",
            "digest_output_mismatch",
            "digest must equal output.sha256.",
        ));
    }
    body.output.validate("/output")?;
    not_svg(&body.output, "/output")?;
    if body.output.host != *event.writer.host() {
        return Err(validation_at(
            "/output/host",
            "output_host_mismatch",
            "The observing steward must own the output blob host.",
        ));
    }
    match &body.method {
        // The checked artifact digest is independent of the check-output digest:
        // Only requires `digest == output.sha256`. SHA-256
        // syntax is already enforced by the `Sha256` wire type.
        ObservationMethod::BlobDigest { .. } => {}
        ObservationMethod::Command { check_id, .. } => {
            validate_text("/method/check_id", check_id, 1, usize::MAX, false)?;
        }
    }
    validate_text("/approval_id", &body.approval_id, 1, usize::MAX, false)
}

fn validate_review_result(body: &ReviewResult) -> Result<()> {
    match body.verdict {
        ReviewVerdict::SendBack => match &body.reason {
            Some(reason) => validate_text("/reason", reason, 1, 500, true)?,
            None if body.comments.is_empty() => {
                return Err(validation_at(
                    "/reason",
                    "send_back_requires_reason",
                    "review.result send_back requires a nonblank reason or at least one comment.",
                ));
            }
            None => {}
        },
        ReviewVerdict::Accept => {
            if let Some(reason) = &body.reason {
                validate_text("/reason", reason, 1, 500, true)?;
            }
            if !body.comments.is_empty() {
                return Err(validation_at(
                    "/comments",
                    "comments_require_send_back",
                    "comments are only allowed with send_back.",
                ));
            }
        }
    }
    if body.save_rule.is_some() && body.verdict != ReviewVerdict::SendBack {
        return Err(validation_at(
            "/save_rule",
            "save_rule_requires_send_back",
            "save_rule is only allowed with send_back.",
        ));
    }
    if let Some(rule) = &body.save_rule {
        validate_rule_set(rule, "/save_rule")?;
    }
    if body.comments.len() > MAX_REVIEW_COMMENTS {
        return Err(validation_at(
            "/comments",
            "too_many_comments",
            format!("A review carries at most {MAX_REVIEW_COMMENTS} comments."),
        ));
    }
    for (index, comment) in body.comments.iter().enumerate() {
        validate_review_comment(comment, &format!("/comments/{index}"))?;
    }
    Ok(())
}

/// Most anchored comments one review may carry.
pub const MAX_REVIEW_COMMENTS: usize = 20;
/// Longest quoted line, in characters, including the trailing ellipsis.
pub const MAX_REVIEW_QUOTE: usize = 200;

/// One anchored review comment, on its own: the host checks it against the
/// delivery's demos before writing.
pub fn validate_review_comment(comment: &ReviewComment, field: &str) -> Result<()> {
    validate_text(&format!("{field}/text"), &comment.text, 1, 300, false)?;
    let anchor = format!("{field}/anchor");
    if usize::from(comment.anchor.demo()) >= 3 {
        return Err(validation_at(
            format!("{anchor}/demo"),
            "demo_out_of_range",
            "A delivery has at most 3 demos; demo must be 0, 1 or 2.",
        ));
    }
    match &comment.anchor {
        ReviewAnchor::DiffLine {
            file, line, quote, ..
        } => {
            validate_text(&format!("{anchor}/file"), file, 1, 300, false)?;
            validate_anchor_line(&anchor, *line)?;
            validate_quote(&anchor, quote)?;
        }
        ReviewAnchor::TranscriptLine { line, quote, .. } => {
            validate_anchor_line(&anchor, *line)?;
            validate_quote(&anchor, quote)?;
        }
        ReviewAnchor::Region { x, y, w, h, .. } => {
            let fits = *w >= 1
                && *h >= 1
                && u32::from(*x) + u32::from(*w) <= 1000
                && u32::from(*y) + u32::from(*h) <= 1000;
            if !fits {
                return Err(validation_at(
                    anchor,
                    "region_out_of_range",
                    "A region is per-mille of the image: w and h at least 1, x+w and y+h at most 1000.",
                ));
            }
        }
    }
    Ok(())
}

fn validate_anchor_line(anchor: &str, line: u32) -> Result<()> {
    if line == 0 {
        return Err(validation_at(
            format!("{anchor}/line"),
            "line_out_of_range",
            "Lines are numbered from 1.",
        ));
    }
    Ok(())
}

/// A quote may be blank (an empty line) but stays one short line.
fn validate_quote(anchor: &str, quote: &str) -> Result<()> {
    let field = format!("{anchor}/quote");
    if quote.chars().count() > MAX_REVIEW_QUOTE {
        return Err(validation_at(
            field,
            "length",
            format!("A quote is at most {MAX_REVIEW_QUOTE} characters."),
        ));
    }
    if quote.chars().any(|c| c.is_control() && c != '\t') {
        return Err(validation_at(
            field,
            "control_character",
            "A quote must not contain control characters other than tab.",
        ));
    }
    Ok(())
}

fn validate_session_continued(body: &SessionContinued) -> Result<()> {
    if body.session == body.previous {
        return Err(validation_at(
            "/previous",
            "session_continues_itself",
            "A session cannot continue itself.",
        ));
    }
    Ok(())
}

/// The note limit shared by `work.progress` and `plan.dropped`.
pub(crate) fn validate_note(note: &str) -> Result<()> {
    validate_text("/note", note, 1, limits::NOTE, false)
}

/// A snooze names one row by its id and ends at most seven days after the
/// event's own time. An `until` at or before `ts` is valid: it ends the snooze.
fn validate_need_snoozed(body: &NeedSnoozed, event: &Event) -> Result<()> {
    let need = &body.need;
    if need.is_empty() || need != need.trim() {
        return Err(validation_at(
            "/need",
            "blank_string",
            "/need must be the id of a Needs-you row.",
        ));
    }
    if need.chars().count() > MAX_NEED_ID_CHARS {
        return Err(validation_at(
            "/need",
            "length",
            format!("/need is at most {MAX_NEED_ID_CHARS} characters."),
        ));
    }
    if need.chars().any(char::is_control) {
        return Err(validation_at(
            "/need",
            "control_character",
            "/need must not contain control characters.",
        ));
    }
    if body.until.millis() - event.ts.millis() > MAX_SNOOZE_MS {
        return Err(validation_at(
            "/until",
            "snooze_too_long",
            "A snooze can last at most seven days.",
        ));
    }
    Ok(())
}

fn validate_session_lost(_body: &SessionLost) -> Result<()> {
    Ok(())
}

fn validate_delivered(_body: &Delivered) -> Result<()> {
    Ok(())
}

fn validate_rule_set(body: &RuleSet, prefix: &str) -> Result<()> {
    validate_text(&join_field(prefix, "text"), &body.text, 1, 500, false)
}

fn validate_policy_set(body: &PolicySet) -> Result<()> {
    validate_budget(&body.budget)?;
    let cells = &body.cells;
    if cells.irreversible_local != Mode::Ask {
        return Err(locked_irreversible("/cells/irreversible_local"));
    }
    if cells.irreversible_wide != Mode::Ask {
        return Err(locked_irreversible("/cells/irreversible_wide"));
    }
    let expected = match body.preset {
        Preset::Balanced => Some(PolicyCells {
            easy_local: Mode::Auto,
            easy_wide: Mode::Assume,
            costly_local: Mode::Assume,
            costly_wide: Mode::Ask,
            irreversible_local: Mode::Ask,
            irreversible_wide: Mode::Ask,
        }),
        Preset::Strict => Some(PolicyCells {
            easy_local: Mode::Ask,
            easy_wide: Mode::Ask,
            costly_local: Mode::Ask,
            costly_wide: Mode::Ask,
            irreversible_local: Mode::Ask,
            irreversible_wide: Mode::Ask,
        }),
        Preset::Autopilot => Some(PolicyCells {
            easy_local: Mode::Auto,
            easy_wide: Mode::Auto,
            costly_local: Mode::Assume,
            costly_wide: Mode::Assume,
            irreversible_local: Mode::Ask,
            irreversible_wide: Mode::Ask,
        }),
        Preset::Custom => None,
    };
    if let Some(expected) = expected {
        check_cell("/cells/easy_local", cells.easy_local, expected.easy_local)?;
        check_cell("/cells/easy_wide", cells.easy_wide, expected.easy_wide)?;
        check_cell(
            "/cells/costly_local",
            cells.costly_local,
            expected.costly_local,
        )?;
        check_cell(
            "/cells/costly_wide",
            cells.costly_wide,
            expected.costly_wide,
        )?;
        check_cell(
            "/cells/irreversible_local",
            cells.irreversible_local,
            expected.irreversible_local,
        )?;
        check_cell(
            "/cells/irreversible_wide",
            cells.irreversible_wide,
            expected.irreversible_wide,
        )?;
        let expected_timeouts = !matches!(body.preset, Preset::Strict);
        if body.allow_timeouts != expected_timeouts {
            return Err(validation_at(
                "/allow_timeouts",
                "preset_timeout_mismatch",
                "allow_timeouts must match the selected preset; use preset=custom to edit it.",
            ));
        }
    }
    Ok(())
}

fn locked_irreversible(field: &str) -> AgendarError {
    validation_at(
        field,
        "locked_irreversible_cell",
        "Irreversible policy cells are locked to ask.",
    )
}

fn check_cell(field: &str, actual: Mode, expected: Mode) -> Result<()> {
    if actual != expected {
        return Err(validation_at(
            field,
            "preset_cells_mismatch",
            "Policy cells must match the selected preset; use preset=custom to edit them.",
        ));
    }
    Ok(())
}

fn validate_budget(budget: &Budget) -> Result<()> {
    if !(1..=1000).contains(&budget.max_speculative_features) {
        return Err(validation_at(
            "/budget/max_speculative_features",
            "budget_bounds",
            "max_speculative_features must be 1..=1000.",
        ));
    }
    if !(1..=100).contains(&budget.max_pending_slices) {
        return Err(validation_at(
            "/budget/max_pending_slices",
            "budget_bounds",
            "max_pending_slices must be 1..=100.",
        ));
    }
    if !(60..=604_800).contains(&budget.max_demo_gap_s) {
        return Err(validation_at(
            "/budget/max_demo_gap_s",
            "budget_bounds",
            "max_demo_gap_s must be 60..=604800.",
        ));
    }
    Ok(())
}

fn validate_policy_evaluation(policy: &PolicyEvaluation, prefix: &str) -> Result<()> {
    if policy.effective < policy.requested {
        return Err(validation_at(
            join_field(prefix, "effective"),
            "effective_below_requested",
            "policy.effective must not be lower than policy.requested.",
        ));
    }
    for (index, hit) in policy.heuristic_hits.iter().enumerate() {
        validate_text(
            &join_field(prefix, &format!("heuristic_hits/{index}")),
            hit,
            1,
            usize::MAX,
            false,
        )?;
    }
    for (index, reason) in policy.budget.reasons.iter().enumerate() {
        validate_text(
            &join_field(prefix, &format!("budget/reasons/{index}")),
            reason,
            1,
            usize::MAX,
            false,
        )?;
    }
    Ok(())
}

fn validate_card(card: &DecisionCard) -> Result<()> {
    validate_text(
        "/card/why_now",
        &card.why_now,
        1,
        limits::CARD_WHY_NOW,
        false,
    )?;
    validate_text(
        "/card/question",
        &card.question,
        1,
        limits::CARD_QUESTION,
        false,
    )?;
    if card.options.len() < 2 || card.options.len() > 4 {
        return Err(validation_at(
            "/card/options",
            "option_count",
            "A decision card must have 2..=4 options.",
        ));
    }
    let mut option_ids = BTreeSet::new();
    for (index, option) in card.options.iter().enumerate() {
        if !option_ids.insert(option.id.as_str()) {
            return Err(validation_at(
                format!("/card/options/{index}/id"),
                "duplicate_option",
                "Decision option IDs must be unique.",
            ));
        }
        let field = format!("/card/options/{index}");
        if option.id.as_str() == crate::model::cards::OWN_OPTION {
            return Err(validation_at(
                format!("{field}/id"),
                "reserved_option",
                "Option id `own` is reserved for the human's own answer.",
            ));
        }
        validate_text(
            &format!("{field}/label"),
            &option.label,
            1,
            limits::OPTION_LABEL,
            false,
        )?;
        validate_text(
            &format!("{field}/consequence"),
            &option.consequence,
            1,
            limits::OPTION_CONSEQUENCE,
            false,
        )?;
        if let Some(cost) = &option.cost {
            validate_text(
                &format!("{field}/cost"),
                cost,
                1,
                limits::OPTION_COST,
                false,
            )?;
        }
        if let Some(eta) = &option.eta {
            eta.validate(&format!("{field}/eta"))?;
        }
        if let Some(icon) = &option.icon
            && !ICON_NAMES.contains(&icon.as_str())
        {
            return Err(validation_at(
                format!("{field}/icon"),
                "unknown_icon",
                format!("Option icon {icon:?} is not in the icon set."),
            )
            .with_hint(format!(
                "Use one of: {}; or leave icon out.",
                ICON_NAMES.join(", ")
            )));
        }
        if let Some(image) = &option.image {
            image.validate(&format!("{field}/image"))?;
            if !IMAGE_EXTS.contains(&image.ext) {
                return Err(validation_at(
                    format!("{field}/image/ext"),
                    "option_image_ext",
                    "An option image must be png, jpg or webp.",
                ));
            }
        }
    }
    if !option_ids.contains(card.recommended.option.as_str()) {
        return Err(validation_at(
            "/card/recommended/option",
            "unknown_recommendation",
            "recommended.option must name an option on this card.",
        ));
    }
    validate_text(
        "/card/recommended/because",
        &card.recommended.because,
        1,
        limits::RECOMMENDED_BECAUSE,
        false,
    )?;
    if card.already_checked.len() > 3 {
        return Err(validation_at(
            "/card/already_checked",
            "facts_limit",
            "already_checked must contain at most 3 facts.",
        ));
    }
    for (index, fact) in card.already_checked.iter().enumerate() {
        validate_fact(&format!("/card/already_checked/{index}"), fact)?;
    }
    if card.done_when.is_empty() || card.done_when.len() > 3 {
        return Err(validation_at(
            "/card/done_when",
            "done_when_count",
            "done_when must contain 1..=3 criteria.",
        ));
    }
    let mut criterion_ids = BTreeSet::new();
    for (index, criterion) in card.done_when.iter().enumerate() {
        if !criterion_ids.insert(criterion.id.as_str()) {
            return Err(validation_at(
                format!("/card/done_when/{index}/id"),
                "duplicate_criterion",
                "done_when must not repeat a criterion ID.",
            ));
        }
        validate_criterion(&format!("/card/done_when/{index}"), criterion)?;
    }
    if let Some(evidence) = &card.evidence {
        validate_card_evidence(evidence)?;
    }
    if let Some(report) = &card.report {
        report.validate("/card/report")?;
        not_svg(report, "/card/report")?;
    }
    Ok(())
}

/// Validate a caller-supplied decision card against the wire limits before any
/// mutation is admitted. This is the same structural validation the decoder
/// applies, exposed so the service can reject an over-limit card without
/// writing an event.
pub fn validate_card_input(card: &DecisionCard) -> Result<()> {
    validate_card(card)
}

fn validate_card_evidence(evidence: &CardEvidence) -> Result<()> {
    match evidence {
        CardEvidence::Bullets { items } => {
            if items.is_empty() || items.len() > 3 {
                return Err(validation_at(
                    "/card/evidence/items",
                    "bullets_count",
                    "Bullets evidence must contain 1..=3 items.",
                ));
            }
            for (index, item) in items.iter().enumerate() {
                validate_text(
                    &format!("/card/evidence/items/{index}"),
                    item,
                    1,
                    120,
                    false,
                )?;
            }
        }
        CardEvidence::Table { rows } => {
            if rows.is_empty() || rows.len() > 4 {
                return Err(validation_at(
                    "/card/evidence/rows",
                    "table_rows",
                    "Table evidence must contain 1..=4 rows.",
                ));
            }
            let columns = rows[0].len();
            if columns == 0 || columns > 4 {
                return Err(validation_at(
                    "/card/evidence/rows/0",
                    "table_columns",
                    "Table evidence rows must contain 1..=4 cells.",
                ));
            }
            for (row_index, row) in rows.iter().enumerate() {
                if row.len() != columns {
                    return Err(validation_at(
                        format!("/card/evidence/rows/{row_index}"),
                        "table_columns",
                        "Table evidence rows must have equal columns.",
                    ));
                }
                for (cell_index, cell) in row.iter().enumerate() {
                    validate_text(
                        &format!("/card/evidence/rows/{row_index}/{cell_index}"),
                        cell,
                        1,
                        120,
                        false,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn validate_criterion(field: &str, criterion: &Criterion) -> Result<()> {
    validate_text(
        &format!("{field}/text"),
        &criterion.text,
        1,
        limits::CRITERION_TEXT,
        false,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CriterionContext {
    Progress,
    Delivery,
}

fn validate_criterion_result(
    field: &str,
    result: &CriterionResult,
    context: CriterionContext,
) -> Result<()> {
    if let Some(evidence) = &result.evidence {
        evidence.validate(&format!("{field}/evidence"))?;
        not_svg(evidence, &format!("{field}/evidence"))?;
    }
    match context {
        CriterionContext::Delivery => {
            if result.claim == Claim::Seen {
                return Err(validation_at(
                    format!("{field}/claim"),
                    "delivery_seen_requires_later_observation",
                    "Delivery criteria must claim agent_says and omit observation.",
                ));
            }
            if result.observation.is_some() {
                return Err(validation_at(
                    format!("{field}/observation"),
                    "delivery_observation_forbidden",
                    "Delivery criteria must omit observation.",
                ));
            }
        }
        CriterionContext::Progress => match result.claim {
            Claim::Seen => {
                if result.observation.is_none() {
                    return Err(validation_at(
                        format!("{field}/observation"),
                        "seen_requires_observation",
                        "claim seen requires a matching observation reference.",
                    ));
                }
            }
            Claim::AgentSays => {
                if result.observation.is_some() {
                    return Err(validation_at(
                        format!("{field}/observation"),
                        "agent_says_forbids_observation",
                        "claim agent_says must omit observation.",
                    ));
                }
            }
        },
    }
    Ok(())
}

fn validate_fact(field: &str, fact: &crate::model::cards::Fact) -> Result<()> {
    validate_text(
        &format!("{field}/text"),
        &fact.text,
        1,
        limits::FACT_TEXT,
        false,
    )?;
    match fact.claim {
        Claim::Seen => {
            if fact.observation.is_none() {
                return Err(validation_at(
                    format!("{field}/observation"),
                    "seen_requires_observation",
                    "claim seen requires a matching observation reference.",
                ));
            }
        }
        Claim::AgentSays => {
            if fact.observation.is_some() {
                return Err(validation_at(
                    format!("{field}/observation"),
                    "agent_says_forbids_observation",
                    "claim agent_says must omit observation.",
                ));
            }
        }
    }
    Ok(())
}

const IMAGE_EXTS: [BlobExt; 3] = [BlobExt::Png, BlobExt::Jpg, BlobExt::Webp];
/// SVG is a project icon only: every other blob path refuses it.
fn not_svg(blob: &BlobRef, field: &str) -> Result<()> {
    if blob.ext == BlobExt::Svg {
        return Err(validation_at(
            format!("{field}/ext"),
            "svg_icon_only",
            "SVG is accepted only as a project icon.",
        ));
    }
    Ok(())
}

/// A project icon may also be SVG; no other media path accepts it.
pub const ICON_EXTS: [BlobExt; 4] = [BlobExt::Png, BlobExt::Jpg, BlobExt::Webp, BlobExt::Svg];
const TEXT_EXTS: [BlobExt; 3] = [BlobExt::Txt, BlobExt::Md, BlobExt::Html];
const VIDEO_EXTS: [BlobExt; 2] = [BlobExt::Mp4, BlobExt::Webm];

fn validate_demo_blob(field: &str, blob: &BlobRef, allowed: &[BlobExt], what: &str) -> Result<()> {
    blob.validate(field)?;
    if !allowed.contains(&blob.ext) {
        let allowed: Vec<&str> = allowed.iter().map(|ext| ext.as_str()).collect();
        return Err(validation_at(
            format!("{field}/ext"),
            "demo_extension_mismatch",
            format!(
                "A {what} requires a {} blob; got `{}`.",
                allowed.join(", "),
                blob.ext.as_str()
            ),
        ));
    }
    Ok(())
}

fn validate_demo_check(field: &str, check: &crate::model::cards::DemoCheck) -> Result<()> {
    if check.argv.is_empty() || check.argv.len() > 32 {
        return Err(validation_at(
            format!("{field}/argv"),
            "check_argv_count",
            "A proposed check needs 1..=32 argv entries.",
        ));
    }
    for (index, argument) in check.argv.iter().enumerate() {
        validate_text(&format!("{field}/argv/{index}"), argument, 1, 4096, false)?;
    }
    validate_text(&format!("{field}/cwd"), &check.cwd, 1, 1024, false)?;
    if !(1..=120).contains(&check.timeout_s) {
        return Err(validation_at(
            format!("{field}/timeout_s"),
            "check_timeout",
            "A proposed check timeout must be 1..=120 seconds.",
        ));
    }
    Ok(())
}

fn validate_demo(field: &str, demo: &Demo) -> Result<()> {
    match demo {
        Demo::Screenshot(body) => {
            validate_demo_blob(
                &format!("{field}/body/blob"),
                &body.blob,
                &IMAGE_EXTS,
                "screenshot demo",
            )?;
            validate_text(&format!("{field}/body/alt"), &body.alt, 1, 160, false)?;
            if let Some(check) = &body.check {
                validate_demo_check(&format!("{field}/body/check"), check)?;
            }
        }
        Demo::CliTranscript(body) => {
            validate_demo_blob(
                &format!("{field}/body/blob"),
                &body.blob,
                &TEXT_EXTS,
                "CLI transcript demo",
            )?;
            if let Some(check) = &body.check {
                validate_demo_check(&format!("{field}/body/check"), check)?;
            }
        }
        Demo::PreviewUrl(body) => {
            validate_preview_url(&format!("{field}/body/url"), &body.url)?;
        }
        Demo::Recording(body) => {
            validate_demo_blob(
                &format!("{field}/body/blob"),
                &body.blob,
                &VIDEO_EXTS,
                "recording demo",
            )?;
            validate_demo_blob(
                &format!("{field}/body/poster"),
                &body.poster,
                &IMAGE_EXTS,
                "recording poster",
            )?;
        }
        Demo::Diff(body) => validate_diff_demo(field, body)?,
    }
    Ok(())
}

/// Most hunks one diff demo may show; it is the core, not the whole change.
pub const DIFF_SHOWN_MAX: usize = 12;
/// Most files listed as not shown.
pub const DIFF_OMITTED_MAX: usize = 200;

fn validate_diff_demo(field: &str, body: &crate::model::cards::DiffDemo) -> Result<()> {
    validate_demo_blob(
        &format!("{field}/body/blob"),
        &body.blob,
        &[BlobExt::Txt],
        "diff demo",
    )?;
    let commit = &body.commit;
    if commit.len() != 40
        || !commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(validation_at(
            format!("{field}/body/commit"),
            "diff_commit",
            "A diff demo commit must be a full 40-character lowercase hex sha.",
        ));
    }
    if body.shown.is_empty() || body.shown.len() > DIFF_SHOWN_MAX {
        return Err(validation_at(
            format!("{field}/body/shown"),
            "diff_shown_count",
            format!("A diff demo shows 1..={DIFF_SHOWN_MAX} hunks."),
        ));
    }
    for (index, shown) in body.shown.iter().enumerate() {
        let at = format!("{field}/body/shown/{index}");
        validate_text(&format!("{at}/path"), &shown.path, 1, 300, false)?;
        validate_text(&format!("{at}/why"), &shown.why, 1, 160, false)?;
        if shown.hunk == 0 {
            return Err(validation_at(
                format!("{at}/hunk"),
                "diff_hunk_index",
                "A hunk index is 1-based.",
            ));
        }
    }
    if body.omitted.len() > DIFF_OMITTED_MAX {
        return Err(validation_at(
            format!("{field}/body/omitted"),
            "diff_omitted_count",
            format!("A diff demo lists at most {DIFF_OMITTED_MAX} omitted files."),
        ));
    }
    for (index, file) in body.omitted.iter().enumerate() {
        validate_text(
            &format!("{field}/body/omitted/{index}/path"),
            &file.path,
            1,
            300,
            false,
        )?;
    }
    Ok(())
}

fn validate_preview_url(field: &str, value: &str) -> Result<()> {
    // A URL limit is technical, not prose: no grace.
    if value.trim().chars().count() > 2048 {
        return Err(validation_at(
            field,
            "length",
            format!("{field} must be at most 2048 characters."),
        ));
    }
    validate_text(field, value, 1, 2048, false)?;
    let url = url::Url::parse(value)
        .map_err(|_| validation_at(field, "preview_url_invalid", "Preview URL must be valid."))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(validation_at(
            field,
            "preview_url_scheme",
            "Preview URL must use http or https.",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(validation_at(
            field,
            "preview_url_credentials",
            "Preview URL must not contain credentials.",
        ));
    }
    if url.host_str().is_none() {
        return Err(validation_at(
            field,
            "preview_url_host",
            "Preview URL must contain a host.",
        ));
    }
    Ok(())
}

fn validate_disclosure(disclosure: &crate::model::cards::Disclosure) -> Result<()> {
    for (name, values) in [
        ("proven", &disclosure.proven),
        ("not_proven_yet", &disclosure.not_proven_yet),
        ("faked_for_now", &disclosure.faked_for_now),
    ] {
        if values.len() > 10 {
            return Err(validation_at(
                format!("/disclosure/{name}"),
                "disclosure_limit",
                "Each disclosure list must contain at most 10 strings.",
            ));
        }
        for (index, value) in values.iter().enumerate() {
            validate_text(&format!("/disclosure/{name}/{index}"), value, 1, 240, true)?;
        }
    }
    Ok(())
}

fn validate_text(field: &str, value: &str, min: usize, max: usize, multiline: bool) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(validation_at(
            field,
            "blank_string",
            format!("{field} must not be blank."),
        ));
    }
    let count = trimmed.chars().count();
    if count < min {
        return Err(validation_at(
            field,
            "length",
            format!("{field} must be at least {min} characters after trimming; got {count}."),
        ));
    }
    if max != usize::MAX && count > grace(max) {
        return Err(validation_at(
            field,
            "length",
            format!(
                "{field} is {count} characters; keep it to {max} (up to {} is accepted).",
                grace(max)
            ),
        )
        .with_hint(format!(
            "Shorten {field} by {} or more characters: keep the essentials and move detail to a note, a report or another field.",
            count - grace(max)
        )));
    }
    for character in value.chars() {
        if character == '\0' {
            return Err(validation_at(
                field,
                "control_character",
                format!("{field} must not contain NUL."),
            ));
        }
        if character == '\n' || character == '\t' {
            if multiline {
                continue;
            }
            return Err(validation_at(
                field,
                "control_character",
                format!("{field} must not contain LF or tab."),
            ));
        }
        if character.is_control() {
            return Err(validation_at(
                field,
                "control_character",
                format!("{field} must not contain control characters."),
            ));
        }
    }
    Ok(())
}

fn join_field(prefix: &str, name: &str) -> String {
    if prefix == "/" {
        format!("/{name}")
    } else {
        format!("{prefix}/{name}")
    }
}

fn validate_body_references(event: &Event) -> Result<()> {
    for (field, id) in body_references(&event.payload) {
        if !event.refs.causes.contains(&id) {
            return Err(validation_at(
                field,
                "body_reference_missing_from_causes",
                "Body event references must also appear in refs.causes.",
            ));
        }
    }
    Ok(())
}

fn body_references(payload: &EventPayload) -> Vec<(String, EventId)> {
    let mut references = Vec::new();
    match payload {
        EventPayload::DecisionAsk(body) => {
            for (index, fact) in body.card.already_checked.iter().enumerate() {
                if let Some(observation) = &fact.observation {
                    references.push((
                        format!("/card/already_checked/{index}/observation"),
                        observation.clone(),
                    ));
                }
            }
            if let Some(policy_event) = &body.policy.policy_event {
                references.push(("/policy/policy_event".to_owned(), policy_event.clone()));
            }
        }
        EventPayload::DecisionAssumed(body) | EventPayload::DecisionAuto(body) => {
            if let Some(policy_event) = &body.policy.policy_event {
                references.push(("/policy/policy_event".to_owned(), policy_event.clone()));
            }
        }
        EventPayload::WorkProgress(body) => {
            for (index, result) in body.evidence.iter().enumerate() {
                if let Some(observation) = &result.observation {
                    references.push((
                        format!("/evidence/{index}/observation"),
                        observation.clone(),
                    ));
                }
            }
        }
        _ => {}
    }
    references
}

// ---------------------------------------------------------------------------
// Strict JSON value decoding: rejects duplicate keys, null and floats.
// ---------------------------------------------------------------------------

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer
            .deserialize_any(StrictValueVisitor)
            .map(StrictValue)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom("floats are not permitted"))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::String(value))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom("null is not permitted"))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom("null is not permitted"))
    }

    fn visit_seq<A>(self, mut access: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = access.next_element::<StrictValue>()? {
            values.push(value.0);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut access: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut object = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate object key `{key}`")));
            }
            let value = access.next_value::<StrictValue>()?;
            object.insert(key, value.0);
        }
        Ok(Value::Object(object))
    }
}

fn json_depth(value: &Value) -> usize {
    match value {
        Value::Array(items) => 1 + items.iter().map(json_depth).max().unwrap_or(0),
        Value::Object(map) => 1 + map.values().map(json_depth).max().unwrap_or(0),
        _ => 1,
    }
}
