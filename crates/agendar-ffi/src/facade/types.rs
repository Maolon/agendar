//! Contract v1 records and enums, field for field as in `agendar.udl`.
//! Ids and RFC 3339 UTC times are strings.
#![forbid(unsafe_code)]

/// What the owner must do to answer a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Guard {
    Easy,
    Choose,
    Confirm,
}

/// Worst-case effective risk of a card or an option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Risk {
    Easy,
    Costly,
    Irreversible,
}

/// Where an answer comes from. `App` allows every guard; `Notification` and
/// `Island` allow only `Guard::Easy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Surface {
    App,
    Notification,
    Island,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NeedKind {
    Decide,
    Rescue,
    Confirm,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RescueAction {
    Handoff,
    Pause,
}

/// How far a written answer got; "uploaded" is the client's own check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReceiptStep {
    Saved,
    AgentHasIt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AskMode {
    Auto,
    Assume,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Confidence {
    Low,
    Med,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepProgress {
    Planned,
    Doing,
    Done,
    Verified,
    Dropped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FactClaim {
    AgentSays,
    Seen,
}

/// Where a step in progress stands after its delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepPhase {
    InReview,
    SentBack,
}

/// What holds a step back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepGate {
    Open,
    AwaitingDecision,
    AwaitingUpstream,
}

/// What one activity line is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivityKind {
    SessionStarted,
    SessionBack,
    SessionEnded,
    ProjectDescribed,
    PlanUpdated,
    Started,
    Finished,
    Dropped,
    Update,
    Asked,
    Answered,
    Applied,
    Withdrawn,
    Report,
    /// A kind this contract version has no name for.
    Other,
}

/// The owner's verdict on a delivery so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReviewState {
    Pending,
    Accepted,
    SentBack,
}

/// The owner's answer: one of the card's options, or their own words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Option { id: String },
    Own { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardStatus {
    Open,
    /// Answered by this device.
    AnsweredHere {
        label: String,
        answer_id: String,
    },
    /// Answered by another host (its host id).
    AnsweredElsewhere {
        label: String,
        host: String,
        answer_id: String,
    },
    /// Nobody answered in time and the default went ahead at `at`.
    Defaulted {
        label: String,
        at: String,
    },
    Withdrawn,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeWindow {
    pub earliest: String,
    pub latest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EtaWindow {
    pub min_s: u32,
    pub max_s: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSeen {
    pub host: String,
    /// The newest event this device has read from that host.
    pub last_event_at: String,
    pub is_self: bool,
}

/// What this device has read of the Vault, and how fresh that is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freshness {
    pub evaluated_at: String,
    pub event_set_digest: Option<String>,
    pub generation: u64,
    pub events: u32,
    pub this_host: String,
    /// Every host with events, newest first.
    pub hosts: Vec<HostSeen>,
    /// Vault-relative writer files the sync app has not downloaded yet.
    pub waiting: Vec<String>,
}

/// One item that needs the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedRow {
    pub id: String,
    pub kind: NeedKind,
    pub project: String,
    pub project_title: String,
    pub node: Option<String>,
    pub step_title: Option<String>,
    /// The event the item is about: for `Decide`, the ask's event id.
    pub source: String,
    pub question: String,
    /// The question as a catalog message (JSON) when the core wrote it.
    pub question_msg_json: Option<String>,
    pub reason: String,
    pub reason_msg_json: Option<String>,
    pub since: String,
    pub waiting_minutes: u64,
    pub stalled_agents: u32,
    pub default_at: Option<String>,
    /// The revision to pass back as `expected_revision`.
    pub revision: String,
    /// Present for `Decide` rows only.
    pub guard: Option<Guard>,
    pub snoozed_until: Option<String>,
}

/// Why an agent is under Stalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StallReason {
    /// It waits on a Needs-you row.
    NeedsYou,
    /// The owner sent its step back and nobody is on it.
    SentBack,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    pub session: String,
    pub project: String,
    pub step_title: Option<String>,
    /// The harness as the core spells it (`claude_code`, `codex`, ...).
    pub harness: String,
    pub since: String,
    /// Present on a stalled row, absent on a moving one.
    pub stall: Option<StallReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoneRow {
    pub id: String,
    pub text: String,
    pub project: String,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NowBoard {
    /// Active needs across projects, most urgent first, snoozed ones left out.
    pub needs: Vec<NeedRow>,
    pub snoozed: Vec<NeedRow>,
    pub moving: Vec<AgentRow>,
    pub stalled: Vec<AgentRow>,
    /// Delivered since `looked_at`; empty without it.
    pub done_since: Vec<DoneRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRow {
    pub slug: String,
    pub title: String,
    pub glyph: String,
    pub needs: u32,
    pub moving: u32,
    pub stalled: u32,
    pub stopped_steps: u32,
    pub last_event: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRow {
    pub node: String,
    pub title: String,
    pub progress: StepProgress,
    pub milestone: Option<String>,
    pub paused: bool,
    pub dropped_by_owner: bool,
    /// The session holding the step, when one does.
    pub holder: Option<String>,
    pub expected_finish: Option<TimeWindow>,
    /// Waiting for the owner's review, or sent back.
    pub phase: Option<StepPhase>,
    pub gate: StepGate,
    pub deadline: Option<String>,
    pub priority: u8,
    /// Nodes of the steps this one comes after, as the plan declares them.
    pub depends_on: Vec<String>,
    /// Seconds past the expected finish; 0 when on time or not estimated.
    pub late_by_s: u64,
}

/// When the whole project finishes (`reduce/forecast.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectForecast {
    pub open_steps: u32,
    pub in_progress: u32,
    pub abandoned: u32,
    pub not_started: u32,
    pub agents: u32,
    pub work_low_ms: u64,
    pub work_high_ms: u64,
    /// Only while an agent is working.
    pub finish: Option<TimeWindow>,
    pub basis_steps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPage {
    pub row: ProjectRow,
    pub goal: String,
    pub brief: String,
    pub steps: Vec<StepRow>,
    pub forecast: Option<ProjectForecast>,
    pub needs: Vec<NeedRow>,
    /// Newest first, at most 40 lines.
    pub activity: Vec<ActivityRow>,
    pub stats: ProjectStats,
    pub calibration: Vec<CalibrationRow>,
    /// Newest first.
    pub deliveries: Vec<DeliveryRow>,
}

/// One line of a project's activity: one event, or a burst by one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityRow {
    pub at: String,
    pub kind: ActivityKind,
    pub text: String,
    pub msg_json: Option<String>,
    pub detail: Option<String>,
    pub node: Option<String>,
    pub step_title: Option<String>,
    /// Who did it, as the sentence names them.
    pub actor: String,
    /// The harness as the core spells it; absent for the owner and Agendar.
    pub harness: Option<String>,
    pub by_owner: bool,
    /// How many events the line folds.
    pub events: u32,
    pub has_report: bool,
}

/// The numbers of the desktop's header row (`reduce/stats.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectStats {
    pub agents_running: u32,
    pub agents_idle: u32,
    pub agents_stalled: u32,
    pub finished_24h: u32,
    pub agent_ms_24h: u64,
    pub median_step_ms: Option<u64>,
    pub timed_steps: u32,
    pub instant_excluded: u32,
}

/// How one harness's estimates compared with what its steps took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalibrationRow {
    pub harness: String,
    pub samples: u32,
    pub ratio_milli: u32,
}

/// One delivery of the project that no newer delivery replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryRow {
    pub delivery_id: String,
    pub name: String,
    pub node: String,
    pub step_title: String,
    pub review: ReviewState,
    /// The Needs-you row that waits on it, while one does.
    pub need_id: Option<String>,
    pub demos: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardOption {
    pub id: String,
    pub label: String,
    pub consequence: String,
    pub cost: Option<String>,
    pub eta: Option<EtaWindow>,
    pub risk: Risk,
    pub external: bool,
    pub has_image: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardFact {
    pub text: String,
    pub claim: FactClaim,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoneCriterion {
    pub id: String,
    pub text: String,
}

/// One decision card, open or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub ask_id: String,
    pub project: String,
    pub project_title: String,
    pub node: String,
    pub step_title: String,
    /// The revision to pass back as `expected_revision`.
    pub revision: String,
    pub guard: Guard,
    /// Effective risk: external effects count as irreversible.
    pub risk: Risk,
    pub external: bool,
    pub risk_words: Vec<String>,
    pub mode: AskMode,
    pub question: String,
    pub why_now: String,
    pub status: CardStatus,
    pub options: Vec<CardOption>,
    pub recommended_option: String,
    pub recommended_confidence: Confidence,
    pub recommended_because: String,
    /// Longest own answer, in Unicode characters.
    pub own_max_chars: u32,
    pub facts: Vec<CardFact>,
    pub done_when: Vec<DoneCriterion>,
    /// Titles of the steps an answer releases.
    pub unblocks: Vec<String>,
    pub stalled_agents: u32,
    pub since: String,
    pub default_at: Option<String>,
    pub default_option: Option<String>,
}

/// What became of one answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub answer_id: String,
    pub ask_id: String,
    pub project: String,
    pub chose_label: String,
    pub own_text: Option<String>,
    pub written_at: String,
    /// Vault-relative path of the file the answer was appended to.
    pub writer_file: String,
    pub step: ReceiptStep,
    pub acked_at: Option<String>,
    /// Titles of the steps the answer released.
    pub released: Vec<String>,
    pub next_need_id: Option<String>,
}
