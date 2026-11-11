//! Derived projection wrappers, revision-token hashing and the derived DTOs of
//! Section 3.5.
//!
//! These are projections, never wire bodies. `PolicySet` and `RuleSet` remain
//! raw bodies without a `revision` field.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{AgendarError, Result};
use crate::model::cards::{
    CheckResult, Criterion, DecisionCard, DecisionOption, Demo, DemoKind, Disclosure, ReviewComment,
};
use crate::model::event::{BudgetUsage, PolicySet, Reply, ReviewResult, RuleSet};
use crate::model::ids::{
    EtaRange, EventId, Harness, HostId, Mode, NodePath, ProjectSlug, RuleId, SessionId, Sha256,
    UtcTime,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyState {
    pub value: PolicySet,
    pub revision: Sha256,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleState {
    pub value: RuleSet,
    pub revision: Sha256,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Progress {
    Planned,
    Doing,
    Done,
    Verified,
    Dropped,
}

impl Progress {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Doing => "doing",
            Self::Done => "done",
            Self::Verified => "verified",
            Self::Dropped => "dropped",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Verified | Self::Dropped)
    }
}

/// The derived phase of a step in progress whose latest work is a delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Delivered; the owner holds it until they review. Never late.
    InReview,
    /// The owner sent the delivery back; the step is open again.
    SentBack,
}

/// Whether an agent still stands behind a step in progress. Derived from the
/// events and `as_of`; a time rule never writes an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lease {
    /// The agent's ETA has not passed, it spoke recently, or the clock is
    /// stopped because it waits on a human.
    Held,
    /// The ETA passed and nothing was heard on the attempt: a label only.
    Stalled,
    /// Nobody stands behind it: its session was lost, or no word came for
    /// [`CLAIM_EXPIRY_S`] after the later of its ETA and its last word.
    Free,
}

/// A claim with nothing heard after its ETA is free this long after the later
/// of its ETA bound and the agent's last word on the attempt (T = 24 h).
pub const CLAIM_EXPIRY_S: i64 = 24 * 60 * 60;

/// A planned step nobody touched for this long (14 days) is `untouched`.
pub const UNTOUCHED_AFTER_S: i64 = 14 * 24 * 60 * 60;

/// The claim of the agent session on a step in progress (doing or sent back).
/// A step in review, paused or handed off has none: it is the owner's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimState {
    pub state: Lease,
    /// When this state began: held, from the last word (or the answer that
    /// restarted the clock); stalled, from the ETA bound; free, from the loss
    /// or the expiry.
    pub since: UtcTime,
    /// The latest event on this attempt by its session: a pulse, a delivery, a
    /// progress event or a question on the step.
    pub last_word: UtcTime,
    /// The open question to the owner that stops the clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_on: Option<EventId>,
}

/// The closed table of corrections: rules by which the board shows something
/// other than the agent's last self-report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionRule {
    /// The owner accepted every live delivery: done.
    Accepted,
    /// The owner sent the delivery back: open again.
    SentBack,
    /// The host reported the session gone: free.
    Lost,
    /// No word on the attempt for T after its ETA: free.
    Silent,
    /// Rule 1b: the claim on an accepted step expired: done.
    ClosedAfterAccept,
}

/// Why the board shows a step differently from its last self-report. Present
/// exactly when the shown status differs from what the agent last said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Correction {
    pub rule: CorrectionRule,
    pub since: UtcTime,
    /// The events the rule rests on, at most two.
    pub evidence: Vec<EventId>,
}

/// The attempt the current one took over from another session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TakenFrom {
    pub attempt: EventId,
    pub session: SessionId,
    /// The start that took it over.
    pub at: UtcTime,
    /// The owner's answer that let it (a takeover the agent asked for).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_by: Option<EventId>,
}

/// The owner let `session` continue a step another agent holds: their
/// current answer to its question chose the `takeover` option. The session
/// starts the step by applying `answer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TakeoverGrant {
    pub session: SessionId,
    pub ask: EventId,
    pub answer: EventId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    Open,
    AwaitingDecision,
    AwaitingUpstream,
}

impl Gate {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::AwaitingDecision => "awaiting_decision",
            Self::AwaitingUpstream => "awaiting_upstream",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    Firm,
    Assumed,
    Tainted,
}

impl Basis {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Firm => "firm",
            Self::Assumed => "assumed",
            Self::Tainted => "tainted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    Live,
    Quiet,
    Lost,
}

impl Liveness {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Quiet => "quiet",
            Self::Lost => "lost",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionLifecycle {
    Open,
    Answered,
    Acked,
    Withdrawn,
    Defaulted,
    Amended,
}

impl DecisionLifecycle {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Answered => "answered",
            Self::Acked => "acked",
            Self::Withdrawn => "withdrawn",
            Self::Defaulted => "defaulted",
            Self::Amended => "amended",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptSource {
    Human,
    Assumed,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedsKind {
    Decide,
    Rescue,
    Confirm,
    Review,
}

impl NeedsKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Decide => "decide",
            Self::Rescue => "rescue",
            Self::Confirm => "confirm",
            Self::Review => "review",
        }
    }
}

/// Human rescue intent. It patches the plan; it never controls a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RescueAction {
    Handoff,
    Pause,
}

impl RescueAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Handoff => "handoff",
            Self::Pause => "pause",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Variance {
    Rework,
    Stopped,
    NoDemo,
    Active,
    Quiet,
}

impl Variance {
    pub const fn rank(self) -> u8 {
        match self {
            Self::Rework => 0,
            Self::Stopped => 1,
            Self::NoDemo => 2,
            Self::Active => 3,
            Self::Quiet => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Pending,
    Accepted,
    SentBack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Seen,
    AgentSays,
    NotProvenYet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PacketKind {
    Answer,
    Pivot,
    Review,
    TimeoutDue,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GraphNodeId(String);

impl GraphNodeId {
    pub fn parse(value: &str) -> Result<Self> {
        match value.split_once(':') {
            Some(("feature", node)) => {
                NodePath::parse(node)?;
            }
            Some(("decision" | "review", id)) => {
                EventId::parse(id)?;
            }
            _ => {
                return Err(AgendarError::validation(format!(
                    "GraphNodeId must be `feature:<node>`, `decision:<id>` or `review:<id>`; got `{value}`."
                )));
            }
        }
        Ok(Self(value.to_owned()))
    }

    pub fn feature(node: &NodePath) -> Self {
        Self(format!("feature:{}", node.as_str()))
    }

    pub fn decision(ask: &EventId) -> Self {
        Self(format!("decision:{}", ask.as_str()))
    }

    pub fn review(delivery: &EventId) -> Self {
        Self(format!("review:{}", delivery.as_str()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphNodeKind {
    Feature,
    Decision,
    Review,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphNode {
    pub id: GraphNodeId,
    pub kind: GraphNodeKind,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feature: Option<NodePath>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub need_id: Option<String>,
    pub state: String,
    pub assumed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphEdge {
    pub from: GraphNodeId,
    pub to: GraphNodeId,
    pub source_event: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeRange {
    pub earliest: UtcTime,
    pub latest: UtcTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EtaState {
    pub agent_range: EtaRange,
    pub anchor: UtcTime,
    pub paused_s: u64,
    pub agent_finish: TimeRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub likely_finish: Option<TimeRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio_milli: Option<u32>,
    pub late_by_s: u64,
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Calibration {
    pub harness: Harness,
    pub samples: u32,
    pub ratio_milli: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureState {
    pub node: NodePath,
    pub title: String,
    pub slice: String,
    pub demo_kind: DemoKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub milestone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<UtcTime>,
    pub priority: u8,
    pub paused: bool,
    pub handoff: bool,
    pub depends_on: Vec<NodePath>,
    /// The declared edges that survived cycle rejection. The rejected edges are
    /// surfaced as rescue rows; the accepted subset is required to reproduce the
    /// canonical graph without re-deriving the causal order.
    pub accepted_depends_on: Vec<NodePath>,
    /// Title, slice and demo kind are all declared; only complete features can be
    /// scheduled.
    pub complete: bool,
    pub field_sources: BTreeMap<String, EventId>,
    pub progress: Progress,
    /// Where a step in progress stands after its delivery: waiting for the
    /// owner's review, or sent back. Derived from the reviews; absent while
    /// the agent is at work and once the step is finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phase>,
    /// The agent's claim on a step in progress; see [`ClaimState`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<ClaimState>,
    /// Present exactly when the shown status differs from the last
    /// self-report; see [`CorrectionRule`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction: Option<Correction>,
    /// The other session's unfinished attempt the current one replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken_from: Option<TakenFrom>,
    /// Agents the owner let continue this step while another agent holds it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub takeover_grants: Vec<TakeoverGrant>,
    pub gate: Gate,
    pub basis: Basis,
    pub liveness: Liveness,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    pub assumptions: Vec<EventId>,
    pub taints: Vec<EventId>,
    pub waits_on: Vec<GraphNodeId>,
    pub unblocks: Vec<NodePath>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta: Option<EtaState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_pulse: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff_event: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff_consumed_by: Option<EventId>,
    pub handoff_pending: bool,
    /// A planned step only agents wrote, nobody started, with no question on
    /// it and no word for [`UNTOUCHED_AFTER_S`]: the app folds it at the end
    /// of the list. A label, never a correction; the step stays work left. Any
    /// owner event on the step (the "Keep" button is one) clears it for good.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub untouched: bool,
    /// Present when the step is dropped and the owner dropped it
    /// (`plan.dropped`); a step an agent dropped has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_drop: Option<OwnerDrop>,
}

/// The owner's drop a dropped step rests on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerDrop {
    pub event: EventId,
    pub at: UtcTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionState {
    pub id: EventId,
    pub node: NodePath,
    pub card: DecisionCard,
    pub mode: Mode,
    pub lifecycle: DecisionLifecycle,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_receipt: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied_answer: Option<EventId>,
    pub answer_history: Vec<EventId>,
    pub ack_history: Vec<EventId>,
    /// Sessions that applied the effective receipt through an ack or an explicit
    /// start/progress reference; the runnable predicate uses it to return the
    /// pending packet before other work.
    pub applied_sessions: Vec<SessionId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_at: Option<UtcTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assumed_at: Option<UtcTime>,
    pub built_on: Vec<NodePath>,
    pub pending_pivot: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub withdrawn_by: Option<EventId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub id: EventId,
    pub ask: EventId,
    pub node: NodePath,
    pub source: ReceiptSource,
    pub chose: DecisionOption,
    pub ruled_out: Vec<DecisionOption>,
    pub done_when: Vec<Criterion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_eta: Option<EtaRange>,
    pub created_at: UtcTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_finish: Option<TimeRange>,
    /// The human's own words when no option fit (`chose.id` is `own`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own_answer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectedCriterion {
    pub criterion: Criterion,
    pub agent_result: CheckResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seen_result: Option<CheckResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<EventId>,
    pub observation_history: Vec<EventId>,
    pub observation_frontier: Vec<EventId>,
    pub provenance: Provenance,
    pub conflict: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryState {
    pub id: EventId,
    pub node: NodePath,
    pub attempt: EventId,
    /// The decision receipt this delivery answers. A delivery of finished work
    /// that no decision asked for has none, and then lists no criteria.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<EventId>,
    pub name: String,
    pub demos: Vec<Demo>,
    pub criteria: Vec<ProjectedCriterion>,
    pub disclosure: Disclosure,
    pub review: ReviewStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_event: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<EventId>,
    pub review_history: Vec<EventId>,
    pub review_frontier: Vec<EventId>,
    pub review_conflict: bool,
    /// The earlier delivery on the same feature this one replaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<EventId>,
    /// The agent's replies to the send-back of `supersedes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replies: Vec<Reply>,
    /// This delivery's latest send-back, kept after it is superseded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_back: Option<SentBackNote>,
}

/// What a send-back said: its reason and anchored comments, in stored order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SentBackNote {
    pub review: EventId,
    pub at: UtcTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<ReviewComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionState {
    pub id: SessionId,
    pub host: HostId,
    pub harness: Harness,
    pub last_event: EventId,
    pub last_event_ts: UtcTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_node: Option<NodePath>,
    pub liveness: Liveness,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost_event: Option<EventId>,
    pub stalled_on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeedsYouItem {
    pub id: String,
    pub kind: NeedsKind,
    pub project: ProjectSlug,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<NodePath>,
    pub source: EventId,
    pub question: String,
    pub since: UtcTime,
    pub stalled_agents: u32,
    pub waiting_minutes: u64,
    pub weight_x2: u8,
    pub score_x2: u64,
    pub reason: String,
    /// `reason` as a catalog message, for the app's language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_msg: Option<crate::model::Msg>,
    /// The question as a catalog message when the core wrote it (rescue,
    /// review); an agent's own question has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_msg: Option<crate::model::Msg>,
    pub unblocks: Vec<NodePath>,
    /// A question whose step was dropped, or done with the owner's accept:
    /// when the step closed, and whether the row has folded out of the active
    /// list ([`MOOT_FOLD_S`] later, still unanswered). A folded question stays
    /// answerable, never notifies and no longer holds its session's steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_step: Option<ClosedStep>,
    /// The owner snoozed this row (`need.snoozed`): hosts hide it until
    /// then. Display only; nothing else in the state depends on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<UtcTime>,
}

/// A question that is still open on a step that has closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosedStep {
    /// When the step was dropped, or accepted and done.
    pub since: UtcTime,
    /// The row left the active Needs-you list ("On closed steps").
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub folded: bool,
}

/// An open question on a closed step folds this long after the step closed
/// (T = 24 h).
pub const MOOT_FOLD_S: i64 = 24 * 60 * 60;

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSummary {
    pub slug: ProjectSlug,
    pub title: String,
    pub brief: String,
    pub goal: String,
    pub glyph: String,
    pub archived: bool,
    /// The project's own icon, when an agent or a human set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<crate::model::BlobRef>,
    pub variance: Variance,
    pub active_agents: u32,
    pub stalled_agents: u32,
    /// Steps that are not finished and whose holder is lost. A project with
    /// live work stays active and carries this count instead of `stopped`.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub stopped_steps: u32,
    pub needs_count: u32,
    pub feature_counts: BTreeMap<Progress, u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_demo: Option<UtcTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event: Option<UtcTime>,
    pub host_as_of: BTreeMap<HostId, UtcTime>,
    pub budget: BudgetUsage,
    pub calibration: Vec<Calibration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectState {
    pub summary: ProjectSummary,
    pub features: BTreeMap<NodePath, FeatureState>,
    pub decisions: BTreeMap<EventId, DecisionState>,
    pub receipts: BTreeMap<EventId, Receipt>,
    pub deliveries: BTreeMap<EventId, DeliveryState>,
    pub rules: Vec<RuleState>,
    pub policy: PolicyState,
    pub sessions: BTreeMap<SessionId, SessionState>,
    pub needs_you: Vec<NeedsYouItem>,
    /// Published reports, newest first (at most [`MAX_REPORTS`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reports: Vec<ReportState>,
}

/// Reports kept per project.
pub const MAX_REPORTS: usize = 50;

/// One published report: a summary for the app, the document in the Vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportState {
    pub id: EventId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodePath>,
    pub title: String,
    pub summary: String,
    pub report: crate::model::BlobRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<crate::model::ReportKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<crate::model::Domain>,
    pub ts: UtcTime,
    pub session: SessionId,
    pub harness: Harness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectSlug>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<EventId>,
    pub message: String,
}

impl Diagnostic {
    pub fn new(
        id: impl Into<String>,
        code: impl Into<String>,
        project: Option<ProjectSlug>,
        event: Option<EventId>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            code: Some(code.into()),
            project,
            event,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub projects: BTreeMap<ProjectSlug, ProjectState>,
    pub diagnostics: Vec<Diagnostic>,
    pub event_set_digest: Sha256,
    pub evaluated_at: UtcTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxPacket {
    pub id: Sha256,
    pub project: ProjectSlug,
    pub node: NodePath,
    pub kind: PacketKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ask: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewResult>,
    /// On a review packet, the delivery reviewed: a redelivery supersedes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<EventId>,
    pub source: EventId,
    pub redo: Vec<NodePath>,
    pub created_at: UtcTime,
}

impl InboxPacket {
    /// Packet identity is stable across hosts: sha256 of canonical JSON
    /// `{kind,project,node,source,redo}`.
    pub fn compute_id(
        kind: PacketKind,
        project: &ProjectSlug,
        node: &NodePath,
        source: &EventId,
        redo: &[NodePath],
    ) -> Sha256 {
        let value = serde_json::json!({
            "kind": kind,
            "project": project,
            "node": node,
            "source": source,
            "redo": redo,
        });
        Sha256::from_bytes(canonical_json(&value).as_bytes())
    }

    /// The pivot instruction for agent-facing delivery paths.
    ///
    /// It names the new receipt and the sorted features to redo. It never asks
    /// the agent to ack its way out of rework: an ack proves the answer was
    /// applied, not that the tainted work was repaired.
    pub fn pivot_text(&self) -> Option<String> {
        if self.kind != PacketKind::Pivot {
            return None;
        }
        let redo: Vec<&str> = self.redo.iter().map(NodePath::as_str).collect();
        let mut text =
            String::from("Pivot: this decision was answered differently from the assumption.");
        if let Some(answer) = &self.answer {
            text.push_str(" New receipt ");
            text.push_str(answer.as_str());
            text.push('.');
        }
        if let Some(receipt) = &self.receipt {
            text.push_str(" Chosen option: ");
            text.push_str(&receipt.chose.label);
            text.push('.');
        }
        if redo.is_empty() {
            text.push_str(" No feature is marked for redo.");
        } else {
            text.push_str(" Redo (restart each with this answer in your basis): ");
            text.push_str(&redo.join(", "));
            text.push('.');
        }
        Some(text)
    }
}

/// Canonical JSON: recursively sorted object keys, no insignificant
/// whitespace, arrays in their semantic order.
pub fn canonical_json(value: &Value) -> String {
    let mut output = String::new();
    write_canonical(value, &mut output);
    output
}

fn write_canonical(value: &Value, output: &mut String) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(boolean) => output.push_str(if *boolean { "true" } else { "false" }),
        Value::Number(number) => output.push_str(&number.to_string()),
        Value::String(string) => {
            output.push_str(
                &serde_json::to_string(string).expect("serializing a string cannot fail"),
            );
        }
        Value::Array(items) => {
            output.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_canonical(item, output);
            }
            output.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            output.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output
                    .push_str(&serde_json::to_string(key).expect("serializing a key cannot fail"));
                output.push(':');
                if let Some(item) = map.get(key) {
                    write_canonical(item, output);
                }
            }
            output.push('}');
        }
    }
}

/// Canonical JSON bytes of a serializable value.
pub fn canonical_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("projection values always serialize")
}

fn revision(parts: &[&str]) -> Sha256 {
    let value = Value::Array(
        parts
            .iter()
            .map(|part| Value::String((*part).to_owned()))
            .collect(),
    );
    Sha256::from_bytes(canonical_json(&value).as_bytes())
}

/// H(`["policy", project, source]`) where source is
/// the winning policy.set event ID or the literal `absent`.
pub fn policy_revision(project: &ProjectSlug, source: Option<&EventId>) -> Sha256 {
    let source = source.map_or("absent", EventId::as_str);
    revision(&["policy", project.as_str(), source])
}

/// H(`["rule", project, rule_id, source]`).
pub fn rule_revision(project: &ProjectSlug, rule_id: &RuleId, source: Option<&EventId>) -> Sha256 {
    let source = source.map_or("absent", EventId::as_str);
    revision(&["rule", project.as_str(), rule_id.as_str(), source])
}

/// Every feature field an agenda edit can patch, in a fixed order.
pub const FEATURE_FIELDS: [&str; 9] = [
    "title",
    "slice",
    "demo_kind",
    "milestone",
    "deadline",
    "priority",
    "paused",
    "handoff",
    "depends_on",
];

/// Every project field an agenda edit can patch, in a fixed order.
pub const PROJECT_FIELDS: [&str; 5] = ["title", "brief", "goal", "glyph", "archived"];

/// Per-field revision token used as the `expected_fields` guard.
///
/// H(`["field", project, node, field, source]`) where `source` is the winning
/// patch event ID or the literal `absent`.
pub fn feature_field_revision(
    project: &ProjectSlug,
    node: &NodePath,
    field: &str,
    source: Option<&EventId>,
) -> Sha256 {
    let source = source.map_or("absent", EventId::as_str);
    revision(&["field", project.as_str(), node.as_str(), field, source])
}

/// Per-field dependency revision token, used as `expected_graph_revision`.
pub fn graph_revision(project: &ProjectSlug, node: &NodePath, source: Option<&EventId>) -> Sha256 {
    let source = source.map_or("absent", EventId::as_str);
    revision(&["graph", project.as_str(), node.as_str(), source])
}

/// Project field revision token.
///
/// The event envelope does not project who wrote a project field, so the token
/// hashes the projected canonical value instead of a source ID; it still changes
/// whenever the field changes and survives a rebuild from JSON. Source IDs are
/// preferred where the projection carries them.
pub fn project_field_revision(
    project: &ProjectSlug,
    field: &str,
    value: &serde_json::Value,
) -> Sha256 {
    let value = canonical_json(value);
    revision(&["project_field", project.as_str(), field, &value])
}

/// Project the field revision map of one feature.
pub fn feature_field_revisions(
    project: &ProjectSlug,
    feature: &FeatureState,
) -> BTreeMap<String, Sha256> {
    FEATURE_FIELDS
        .iter()
        .map(|field| {
            let source = feature.field_sources.get(*field);
            (
                (*field).to_owned(),
                feature_field_revision(project, &feature.node, field, source),
            )
        })
        .collect()
}

/// Project the field revision map of one project declaration.
pub fn project_field_revisions(
    project: &ProjectSlug,
    summary: &ProjectSummary,
) -> BTreeMap<String, Sha256> {
    let values: [(&str, serde_json::Value); 5] = [
        ("title", Value::String(summary.title.clone())),
        ("brief", Value::String(summary.brief.clone())),
        ("goal", Value::String(summary.goal.clone())),
        ("glyph", Value::String(summary.glyph.clone())),
        ("archived", Value::Bool(summary.archived)),
    ];
    values
        .into_iter()
        .map(|(field, value)| {
            (
                field.to_owned(),
                project_field_revision(project, field, &value),
            )
        })
        .collect()
}
