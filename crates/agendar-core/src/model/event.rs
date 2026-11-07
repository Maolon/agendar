//! Envelope, event bodies and pure source-path grammar.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::error::{AgendarError, Result};
use crate::model::cards::{
    CheckResult, Criterion, CriterionResult, DecisionCard, Demo, DemoKind, Disclosure,
    ObservationMethod, ReviewVerdict,
};
use crate::model::ids::{
    BlobRef, CriterionId, Domain, EtaRange, EventId, Harness, HostId, Mode, NodePath, OptionId,
    ProjectSlug, Reversibility, RuleId, SessionId, Sha256, UtcTime, WriterId, WriterRole,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePath {
    pub project: ProjectSlug,
    pub writer: WriterId,
    pub segment: u32,
    pub relative_path: String,
}

impl SourcePath {
    /// Parse the exact `projects/<slug>/<host>/<writer-file>` grammar from
    /// Without touching the filesystem.
    pub fn from_relative_path(path: &str) -> Result<Self> {
        if path.is_empty() {
            return Err(source_error("Source path must not be blank."));
        }
        if path.starts_with('/') || path.contains('\\') || path.contains('\0') {
            return Err(source_error(
                "Source path must be a relative slash-separated path.",
            ));
        }
        if path.chars().any(char::is_control) {
            return Err(source_error(
                "Source path must not contain control characters.",
            ));
        }
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() != 4 {
            return Err(source_error(
                "Source path must be `projects/<slug>/<host>/<writer-file>`.",
            ));
        }
        if parts[0] != "projects" {
            return Err(source_error("Source path must start with `projects/`."));
        }
        if parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
        {
            return Err(source_error(
                "Source path must not contain empty, `.` or `..` segments.",
            ));
        }
        let project = ProjectSlug::parse(parts[1])?;
        let host = HostId::parse(parts[2])?;
        let (role, segment) = parse_writer_file(parts[3])?;
        Ok(Self {
            project,
            writer: WriterId::new(host, role),
            segment,
            relative_path: path.to_owned(),
        })
    }
}

fn source_error(message: &str) -> AgendarError {
    AgendarError::validation_at("/source", "invalid_source_path", message)
}

fn parse_writer_file(filename: &str) -> Result<(WriterRole, u32)> {
    let Some(stem) = filename.strip_suffix(".jsonl") else {
        return Err(source_error(
            "Writer file must end with the exact `.jsonl` suffix.",
        ));
    };
    if stem.is_empty() {
        return Err(source_error("Writer file stem must not be blank."));
    }
    let (base, segment) = match stem.rsplit_once('.') {
        Some((base, suffix)) => {
            if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(source_error(
                    "Writer segment suffix must be decimal digits.",
                ));
            }
            if suffix.len() > 1 && suffix.starts_with('0') {
                return Err(source_error(
                    "Writer segment suffix must not have leading zeros.",
                ));
            }
            let segment = suffix
                .parse::<u32>()
                .map_err(|_| source_error("Writer segment suffix is too large."))?;
            if segment < 2 {
                return Err(source_error(
                    "Writer segment suffix must be >= 2; the base file is segment 1.",
                ));
            }
            (base, segment)
        }
        None => (stem, 1),
    };
    if base.is_empty() {
        return Err(source_error("Writer file base name must not be blank."));
    }
    let role = if base == "operator" {
        WriterRole::Operator
    } else if base == "steward" {
        WriterRole::Steward
    } else if let Some(session) = base.strip_prefix("s-") {
        WriterRole::Agent(SessionId::parse(session)?)
    } else {
        return Err(source_error(
            "Writer file must be `operator.jsonl`, `steward.jsonl` or `s-<SessionId>.jsonl`.",
        ));
    };
    Ok((role, segment))
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Refs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<NodePath>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ask: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<EventId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes: Vec<EventId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub basis: Vec<EventId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Event {
    pub v: u8,
    pub id: EventId,
    pub seq: u64,
    pub writer: WriterId,
    pub ts: UtcTime,
    pub project: ProjectSlug,
    pub refs: Refs,
    #[serde(flatten)]
    pub payload: EventPayload,
}

impl Event {
    pub fn kind(&self) -> &'static str {
        self.payload.kind()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body")]
#[allow(clippy::large_enum_variant)]
pub enum EventPayload {
    #[serde(rename = "project.declared")]
    ProjectDeclared(ProjectDeclared),
    #[serde(rename = "plan.feature")]
    PlanFeature(PlanFeature),
    #[serde(rename = "work.progress")]
    WorkProgress(WorkProgress),
    #[serde(rename = "pulse")]
    Pulse(Pulse),
    #[serde(rename = "decision.ask")]
    DecisionAsk(DecisionAsk),
    #[serde(rename = "decision.assumed")]
    DecisionAssumed(DecisionChoice),
    #[serde(rename = "decision.auto")]
    DecisionAuto(DecisionChoice),
    #[serde(rename = "decision.withdrawn")]
    DecisionWithdrawn(DecisionWithdrawn),
    #[serde(rename = "decision.answer")]
    DecisionAnswer(DecisionAnswer),
    #[serde(rename = "decision.ack")]
    DecisionAck(DecisionAck),
    #[serde(rename = "delivery")]
    Delivery(Delivery),
    #[serde(rename = "evidence.observed")]
    EvidenceObserved(EvidenceObserved),
    #[serde(rename = "review.result")]
    ReviewResult(ReviewResult),
    #[serde(rename = "rule.set")]
    RuleSet(RuleSet),
    #[serde(rename = "policy.set")]
    PolicySet(PolicySet),
    #[serde(rename = "session.lost")]
    SessionLost(SessionLost),
    #[serde(rename = "session.continued")]
    SessionContinued(SessionContinued),
    #[serde(rename = "delivered")]
    Delivered(Delivered),
    #[serde(rename = "report.published")]
    ReportPublished(ReportPublished),
}

impl EventPayload {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::ProjectDeclared(_) => "project.declared",
            Self::PlanFeature(_) => "plan.feature",
            Self::WorkProgress(_) => "work.progress",
            Self::Pulse(_) => "pulse",
            Self::DecisionAsk(_) => "decision.ask",
            Self::DecisionAssumed(_) => "decision.assumed",
            Self::DecisionAuto(_) => "decision.auto",
            Self::DecisionWithdrawn(_) => "decision.withdrawn",
            Self::DecisionAnswer(_) => "decision.answer",
            Self::DecisionAck(_) => "decision.ack",
            Self::Delivery(_) => "delivery",
            Self::EvidenceObserved(_) => "evidence.observed",
            Self::ReviewResult(_) => "review.result",
            Self::RuleSet(_) => "rule.set",
            Self::PolicySet(_) => "policy.set",
            Self::SessionLost(_) => "session.lost",
            Self::SessionContinued(_) => "session.continued",
            Self::Delivered(_) => "delivered",
            Self::ReportPublished(_) => "report.published",
        }
    }

    pub fn agent(&self) -> Option<&Agent> {
        match self {
            Self::ProjectDeclared(body) => body.agent.as_ref(),
            Self::PlanFeature(body) => body.agent.as_ref(),
            Self::WorkProgress(body) => Some(&body.agent),
            Self::Pulse(body) => Some(&body.agent),
            Self::DecisionAsk(body) => Some(&body.agent),
            Self::DecisionAssumed(body) | Self::DecisionAuto(body) => Some(&body.agent),
            Self::DecisionWithdrawn(body) => Some(&body.agent),
            Self::DecisionAck(body) => Some(&body.agent),
            Self::Delivery(body) => Some(&body.agent),
            Self::ReportPublished(body) => Some(&body.agent),
            Self::DecisionAnswer(_)
            | Self::EvidenceObserved(_)
            | Self::ReviewResult(_)
            | Self::RuleSet(_)
            | Self::PolicySet(_)
            | Self::SessionLost(_)
            | Self::SessionContinued(_)
            | Self::Delivered(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub session: SessionId,
    pub harness: Harness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectDeclared {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<Agent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glyph: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    /// The project's own icon (png, jpg or webp), shown in full colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<BlobRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DeadlineUpdate {
    Set { value: UtcTime },
    Clear,
}

impl<'de> Deserialize<'de> for DeadlineUpdate {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let Some(map) = value.as_object() else {
            return Err(serde::de::Error::custom("deadline must be an object"));
        };
        let Some(op) = map.get("op").and_then(Value::as_str) else {
            return Err(serde::de::Error::custom("deadline.op must be a string"));
        };
        match op {
            "set" => {
                if map.len() != 2 || !map.contains_key("value") {
                    return Err(serde::de::Error::custom(
                        "deadline set must contain exactly `op` and `value`",
                    ));
                }
                let value = serde_json::from_value(map["value"].clone())
                    .map_err(serde::de::Error::custom)?;
                Ok(Self::Set { value })
            }
            "clear" => {
                if map.len() != 1 {
                    return Err(serde::de::Error::custom(
                        "deadline clear must contain exactly `op`",
                    ));
                }
                Ok(Self::Clear)
            }
            _ => Err(serde::de::Error::custom("unknown deadline op")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanFeature {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<Agent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<NodePath>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub demo_kind: Option<DemoKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub milestone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<DeadlineUpdate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkStatus {
    Started,
    Done,
    Verified,
    Dropped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkProgress {
    pub agent: Agent,
    pub status: WorkStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta: Option<EtaRange>,
    pub evidence: Vec<CriterionResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pulse {
    pub agent: Agent,
    pub message: String,
    pub boundary: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta: Option<EtaRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionAsk {
    pub agent: Agent,
    pub card: DecisionCard,
    pub policy: PolicyEvaluation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChoiceReason {
    Policy,
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionChoice {
    pub agent: Agent,
    pub option: OptionId,
    pub reason: ChoiceReason,
    pub policy: PolicyEvaluation,
}

/// A report: what the board cannot show in a line (an investigation, a
/// comparison), as a short summary in the app and the full document in the
/// Vault, opened in the human's own browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportPublished {
    pub agent: Agent,
    pub title: String,
    pub summary: String,
    /// The document: html, md or txt.
    pub report: BlobRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionWithdrawn {
    pub agent: Agent,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionAnswer {
    pub option: OptionId,
    pub done_when: Vec<Criterion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_eta: Option<EtaRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The human's own words, when `option` is the reserved `own`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own_answer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionAck {
    pub agent: Agent,
    pub applied_option: OptionId,
    pub restarted: Vec<NodePath>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub agent: Agent,
    pub name: String,
    pub demos: Vec<Demo>,
    pub criteria: Vec<CriterionResult>,
    pub disclosure: Disclosure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceObserved {
    pub criterion: CriterionId,
    pub method: ObservationMethod,
    pub started_at: UtcTime,
    pub finished_at: UtcTime,
    pub result: CheckResult,
    pub digest: Sha256,
    pub output: BlobRef,
    pub approval_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewResult {
    pub verdict: ReviewVerdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub save_rule: Option<RuleSet>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSet {
    pub rule_id: RuleId,
    pub text: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    Balanced,
    Strict,
    Autopilot,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyCells {
    pub easy_local: Mode,
    pub easy_wide: Mode,
    pub costly_local: Mode,
    pub costly_wide: Mode,
    pub irreversible_local: Mode,
    pub irreversible_wide: Mode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub max_speculative_features: u32,
    pub max_pending_slices: u32,
    pub max_demo_gap_s: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySet {
    pub preset: Preset,
    pub cells: PolicyCells,
    pub domain_floor: BTreeMap<Domain, Mode>,
    pub budget: Budget,
    pub require_human_review: bool,
    pub allow_timeouts: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LostReason {
    Exited,
    PidReused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionLost {
    pub session: SessionId,
    pub process_start: u64,
    pub reason: LostReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

/// A host-attested continuation: a new session of the same agent conversation
/// on the same computer (`claude --resume`) takes over the steps its lost
/// predecessor was working on. Written by the steward, never by an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionContinued {
    pub session: SessionId,
    pub previous: SessionId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveredOutcome {
    Accepted,
    InboxRead,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivered {
    pub session: SessionId,
    pub packet_id: Sha256,
    pub transport: Harness,
    pub outcome: DeliveredOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetUsage {
    pub speculative_features: u32,
    pub pending_slices: u32,
    pub demo_gap_s: u64,
    pub blocked: bool,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyEvaluation {
    pub requested: Mode,
    pub effective: Mode,
    pub reversibility: Reversibility,
    pub external: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_event: Option<EventId>,
    pub heuristic_hits: Vec<String>,
    pub budget: BudgetUsage,
}
