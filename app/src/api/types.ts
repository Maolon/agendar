import type { Msg } from "../i18n/msg";
/**
 * Typed mirror of the host's wire contract.
 *
 * These are projections and IPC payloads, never events. All keys are
 * snake_case; optional fields are either absent or `undefined`, never null.
 * The frontend must not invent fields: if a projection is missing here, the
 * view renders "Not declared" rather than guessing.
 */

export type Ulid = string;
export type EventId = Ulid;
export type SessionId = Ulid;
export type VaultId = Ulid;
export type HostId = string;
export type ProjectSlug = string;
export type NodePath = string;
export type Sha256 = string;
export type UtcTime = string;
export type OptionId = string;
export type CriterionId = string;
export type RuleId = string;

export type Harness =
  | "claude_code"
  | "codex"
  | "generic"
  | "pi"
  | "herdr"
  | "opencode"
  | "cursor"
  | "dsh"
  | "agy"
  | "gemini";
export type Reversibility = "easy" | "costly" | "irreversible";
export type Reach = "local" | "wide";
export type Domain = "ux" | "arch" | "code";
export type Mode = "auto" | "assume" | "ask";
export type Confidence = "low" | "med" | "high";
export type Claim = "agent_says" | "seen";
export type CheckResult = "passed" | "failed" | "not_proven";
export type DemoKind = "screenshot" | "cli_transcript" | "preview_url" | "recording";
export type Progress = "planned" | "doing" | "done" | "verified" | "dropped";
export type Gate = "open" | "awaiting_decision" | "awaiting_upstream";
export type Basis = "firm" | "assumed" | "tainted";
export type Liveness = "live" | "quiet" | "lost";
export type DecisionLifecycle = "open" | "answered" | "acked" | "withdrawn" | "defaulted" | "amended";
export type ReceiptSource = "human" | "assumed" | "auto";
export type ReviewState = "pending" | "accepted" | "sent_back";
export type Provenance = "seen" | "agent_says" | "not_proven_yet";
export type NeedKind = "decide" | "rescue" | "confirm" | "review";
export type Variance = "rework" | "stopped" | "no_demo" | "active" | "quiet";
export type Health = "healthy" | "degraded" | "offline";
export type NotificationPermission = "unknown" | "granted" | "denied";
export type ActionAvailability = "unknown" | "available" | "unavailable";
export type Theme = "system" | "light" | "dark";
export type BriefSection =
  | "delivered"
  | "coming_up"
  | "needs_you"
  | "if_you_wait"
  | "by_default"
  | "at_risk";
export type Preset = "balanced" | "strict" | "autopilot" | "custom";
export type Guard = "easy" | "choose" | "confirm";

export interface EtaRange {
  min_s: number;
  max_s: number;
}

export interface TimeRange {
  earliest: UtcTime;
  latest: UtcTime;
}

export interface BlobRef {
  host: HostId;
  sha256: Sha256;
  ext: string;
  bytes: number;
}

export interface Agent {
  session: SessionId;
  harness: Harness;
}

export interface Criterion {
  id: CriterionId;
  text: string;
}

export interface CriterionResult {
  criterion: CriterionId;
  status: CheckResult;
  claim: Claim;
  evidence?: BlobRef;
  observation?: EventId;
}

export interface Disclosure {
  proven: string[];
  not_proven_yet: string[];
  faked_for_now: string[];
}

/**
 * A check the delivery proposes for one of its criteria (model `DemoCheck`).
 * Approved on screen, then run; never executed on render.
 */
export interface DemoCheck {
  criterion: CriterionId;
  argv: string[];
  cwd: string;
  timeout_s: number;
}

/**
 * Demo bodies carry kind-specific model fields; members the app does not
 * render stay opaque rather than being guessed.
 */
export interface DemoBody {
  check?: DemoCheck;
  [member: string]: unknown;
}

export interface Demo {
  kind: DemoKind;
  body: DemoBody;
}

export interface DecisionOption {
  id: OptionId;
  label: string;
  consequence: string;
  reversibility: Reversibility;
  external: boolean;
  cost?: string;
  eta?: EtaRange;
  icon?: string;
  /** A picture of this option; shown side by side and at full size. */
  image?: BlobRef;
}

export interface Recommendation {
  option: OptionId;
  confidence: Confidence;
  because: string;
}

export interface Fact {
  text: string;
  claim: Claim;
  observation?: EventId;
}

export type CardEvidence =
  | { kind: "bullets"; items: string[] }
  | { kind: "table"; rows: string[][] };

export type OnTimeout =
  | { kind: "hold" }
  | { kind: "proceed_as_assumption"; after_s: number };

export interface DecisionCard {
  why_now: string;
  question: string;
  reversibility: Reversibility;
  reach: Reach;
  external: boolean;
  domain: Domain;
  options: DecisionOption[];
  recommended: Recommendation;
  already_checked: Fact[];
  done_when: Criterion[];
  evidence?: CardEvidence;
  on_timeout: OnTimeout;
  report?: BlobRef;
}

// ---------------------------------------------------------------------------
// Derived projections

export interface Diagnostic {
  id: string;
  code: string;
  project?: ProjectSlug;
  event?: EventId;
  message: string;
}

export interface State {
  projects: Record<ProjectSlug, ProjectState>;
  diagnostics: Diagnostic[];
  event_set_digest: Sha256;
  evaluated_at: UtcTime;
}

export interface PolicyCells {
  easy_local: Mode;
  easy_wide: Mode;
  costly_local: Mode;
  costly_wide: Mode;
  irreversible_local: Mode;
  irreversible_wide: Mode;
}

export interface Budget {
  max_speculative_features: number;
  max_pending_slices: number;
  max_demo_gap_s: number;
}

export interface BudgetUsage {
  speculative_features: number;
  pending_slices: number;
  demo_gap_s: number;
  blocked: boolean;
  reasons: string[];
}

export interface PolicySet {
  preset: Preset;
  cells: PolicyCells;
  domain_floor: Partial<Record<Domain, Mode>>;
  budget: Budget;
  require_human_review: boolean;
  allow_timeouts: boolean;
}

export interface PolicyState {
  value: PolicySet;
  revision: Sha256;
}

export interface RuleSet {
  rule_id: RuleId;
  text: string;
  enabled: boolean;
}

export interface RuleState {
  value: RuleSet;
  revision: Sha256;
}

export interface EtaState {
  agent_range: EtaRange;
  anchor: UtcTime;
  paused_s: number;
  agent_finish: TimeRange;
  likely_finish?: TimeRange;
  ratio_milli?: number;
  late_by_s: number;
  paused: boolean;
}

export interface Calibration {
  harness: Harness;
  samples: number;
  ratio_milli: number;
}

export interface FeatureState {
  node: NodePath;
  title: string;
  slice: string;
  demo_kind: DemoKind;
  milestone?: string;
  deadline?: UtcTime;
  priority: number;
  paused: boolean;
  handoff: boolean;
  depends_on: NodePath[];
  field_sources: Record<string, EventId>;
  progress: Progress;
  gate: Gate;
  basis: Basis;
  liveness: Liveness;
  attempt?: EventId;
  session?: SessionId;
  assumptions: EventId[];
  taints: EventId[];
  waits_on: string[];
  unblocks: NodePath[];
  eta?: EtaState;
  last_pulse?: EventId;
  handoff_event?: EventId;
  handoff_consumed_by?: EventId;
  handoff_pending?: boolean;
  /** expected_fields for update_feature. */
  field_revisions: Record<string, Sha256>;
  /** expected_graph_revision for dependency edits. */
  graph_revision: Sha256;
}

export interface DecisionState {
  id: EventId;
  node: NodePath;
  card: DecisionCard;
  mode: Mode;
  lifecycle: DecisionLifecycle;
  effective_receipt?: EventId;
  applied_answer?: EventId;
  answer_history: EventId[];
  ack_history: EventId[];
  default_at?: UtcTime;
  assumed_at?: UtcTime;
  built_on: NodePath[];
  pending_pivot: boolean;
  superseded_by?: EventId;
  withdrawn_by?: EventId;
  /** expected_decision_revision for prepare_answer. */
  revision: Sha256;
}

export interface Receipt {
  id: EventId;
  ask: EventId;
  node: NodePath;
  source: ReceiptSource;
  chose: DecisionOption;
  ruled_out: DecisionOption[];
  done_when: Criterion[];
  expected_eta?: EtaRange;
  created_at: UtcTime;
  expected_finish?: TimeRange;
}

export interface ProjectedCriterion {
  criterion: Criterion;
  agent_result: CheckResult;
  seen_result?: CheckResult;
  observation?: EventId;
  provenance: Provenance;
  conflict: boolean;
  observation_history: EventId[];
  observation_frontier: EventId[];
}

export interface DeliveryState {
  id: EventId;
  node: NodePath;
  attempt: EventId;
  receipt: EventId;
  name: string;
  demos: Demo[];
  criteria: ProjectedCriterion[];
  disclosure: Disclosure;
  review: ReviewState;
  review_event?: EventId;
  superseded_by?: EventId;
  review_history: EventId[];
  review_frontier: EventId[];
  review_conflict: boolean;
  /**
   * This host's stored check proposals (`checks::proposals`), projected by the
   * app. Local metadata, not Vault truth: another host may propose none.
   */
  check_proposals?: DemoCheck[];
  /** expected_delivery_revision for review_delivery. */
  revision: Sha256;
}

export interface SessionState {
  id: SessionId;
  host: HostId;
  harness: Harness;
  last_event: EventId;
  last_event_ts: UtcTime;
  current_node?: NodePath;
  liveness: Liveness;
  lost_event?: EventId;
  stalled_on: EventId[];
}

export interface NeedsYouItem {
  id: string;
  kind: NeedKind;
  project: ProjectSlug;
  node?: NodePath;
  source: EventId;
  question: string;
  since: UtcTime;
  stalled_agents: number;
  waiting_minutes: number;
  weight_x2: number;
  score_x2: number;
  reason: string;
  /** `reason` in the chosen language (core catalog message). */
  reason_msg?: Msg;
  /** The question as a catalog message when the core wrote it. */
  question_msg?: Msg;
  unblocks: NodePath[];
  /** expected_rescue_revision for resolve_rescue. */
  revision: Sha256;
}

export interface ProjectSummary {
  slug: ProjectSlug;
  title: string;
  brief: string;
  goal: string;
  glyph: string;
  archived: boolean;
  /** The project's own icon, shown in full colour. */
  icon?: BlobRef;
  variance: Variance;
  active_agents: number;
  stalled_agents: number;
  needs_count: number;
  feature_counts: Partial<Record<Progress, number>>;
  last_demo?: UtcTime;
  last_event?: UtcTime;
  host_as_of: Record<HostId, UtcTime>;
  budget: BudgetUsage;
  calibration: Calibration[];
}

export interface ProjectState {
  summary: ProjectSummary;
  features: Record<NodePath, FeatureState>;
  decisions: Record<EventId, DecisionState>;
  receipts: Record<EventId, Receipt>;
  deliveries: Record<EventId, DeliveryState>;
  rules: RuleState[];
  policy: PolicyState;
  sessions: Record<SessionId, SessionState>;
  needs_you: NeedsYouItem[];
  /** Published reports, newest first. */
  reports?: ReportState[];
  /** expected_fields for update_project. */
  field_revisions: Record<string, Sha256>;
  /** Host-local activity feed, newest first; absent from older hosts. */
  activity?: ActivityItem[];
  /** Host-local header statistics; absent from older hosts. */
  stats?: ProjectStats;
  /** Host-local: per unacked question, whether an agent hears the answer now. */
  listening?: Record<EventId, Listening>;
}

/** Project pace for the header (a host projection, not State). */
export interface ProjectStats {
  /** Agents with a step in progress now. */
  agents_running: number;
  /** Connected agents with nothing in progress. */
  agents_idle: number;
  agents_stalled: number;
  finished_24h: number;
  agent_ms_24h: number;
  median_step_ms?: number;
  timed_steps: number;
  instant_excluded: number;
  /** Tokens the project's agents used in 24h, from their local logs. */
  tokens?: TokenUsage;
  /** When the whole project finishes; absent with too few finished steps. */
  forecast?: ProjectForecast;
}

export interface TokenUsage {
  /** New input, cache writes included. */
  input: number;
  /** Input served from the prompt cache. */
  cached: number;
  output: number;
}

export interface ProjectForecast {
  open_steps: number;
  /** Agents working now; the work is split between them. */
  agents: number;
  /** Remaining agent work [low, high] in ms. */
  work_ms: [number, number];
  /** Clock window, only while an agent works. */
  finish?: TimeRange;
  /** What one not-started step counts for [low, high] in ms. */
  step_ms: [number, number];
  basis_steps: number;
}

/** One human-readable line of project activity (a host projection, not State). */
export interface ActivityItem {
  /** Time of the newest event folded into this line. */
  ts: UtcTime;
  actor: string;
  actor_kind: "agent" | "human" | "system";
  icon: string;
  text: string;
  msg?: Msg;
  detail?: string | null;
  node?: NodePath | null;
  count: number;
  /** The report document this line opens. */
  report?: BlobRef;
}

/** A report: a summary here, the document opened in the person's browser. */
export interface ReportState {
  id: EventId;
  node?: NodePath;
  title: string;
  summary: string;
  report: BlobRef;
  ts: UtcTime;
  session: SessionId;
  harness: Harness;
}

/** How one agent harness is connected to Agendar (Settings › Agents). */
export interface Binding {
  harness: Harness;
  kind: "hooks" | "mcp" | "manual";
  state: "bound" | "not_bound" | "stale" | "unreadable" | "manual";
  /** The harness's executable is on this computer. */
  found: boolean;
  path?: string;
  detail: string;
  /** Newest event from an agent of this kind on this computer (skill or CLI). */
  last_seen?: UtcTime;
}

export interface GetBindingsData {
  bindings: Binding[];
  cli_path: string;
}

export interface SetBindingRequest {
  harness: Harness;
  action: "bind" | "unbind";
}

export interface SetBindingData {
  binding: Binding;
}

export interface OpenReportRequest {
  project: ProjectSlug;
  blob: BlobRef;
}

export interface OpenReportData {
  opened: boolean;
}

// ---------------------------------------------------------------------------
// Graph; geometry is frontend-owned.

export interface GraphNode {
  id: string;
  kind: "feature" | "decision" | "review";
  label: string;
  feature?: NodePath;
  need_id?: string;
  state: string;
  assumed: boolean;
}

export interface GraphEdge {
  from: string;
  to: string;
  source_event: EventId;
}

export interface Graph {
  nodes: GraphNode[];
  edges: GraphEdge[];
}

// ---------------------------------------------------------------------------
// Snapshot and local status

export interface BriefRow {
  id: string;
  text: string;
  msg?: Msg;
  detail?: string;
  detail_msg?: Msg;
  target: { kind: "need" | "project"; id: string };
  source_ids: EventId[];
}

export type BriefData = Record<BriefSection, BriefRow[]>;

export interface LocalDiagnostic {
  code: string;
  message: string;
  path?: string;
  retryable: boolean;
}

export type AutomaticWake = "unsupported" | "prerequisite_missing" | "unknown" | "available";

export interface AdapterCapabilities {
  harness: Harness;
  running: boolean;
  ended_turn: boolean;
  new_session: boolean;
  queue_supported: boolean;
  automatic_wake: AutomaticWake;
  detail: string;
}

export interface LocalStatus {
  host: HostId;
  received_at: UtcTime;
  health: Health;
  diagnostics: LocalDiagnostic[];
  capabilities: AdapterCapabilities[];
  notifications: NotificationPermission;
  notification_actions: { availability: ActionAvailability; reason?: string };
  /** Why the system refused the last notification, when it said so. */
  notification_error?: string;
}

export interface TestNoticeData {
  sent: boolean;
  error?: string;
}

export interface Preferences {
  theme: Theme;
  motion: boolean;
  snoozed: Record<string, UtcTime>;
  /** Hand-off items closed for good on this computer (their steps are untouched). */
  dismissed?: string[];
  last_looked_at?: UtcTime;
  last_looked_ids: EventId[];
  /** expected_revision for set_preferences. */
  revision: Sha256;
  /** Local notification consent; not the OS permission state. */
  notifications?: boolean;
  /** UI language: `auto` follows the system. */
  language?: "auto" | "en" | "zh-Hans" | "zh-Hant";
}

export interface NotificationFocus {
  navigation_epoch: Ulid;
  sequence: number;
  vault_id: VaultId;
  project: ProjectSlug;
  item_id: string;
  status: string;
}

export interface Snapshot {
  v: 1;
  epoch: Ulid;
  navigation_epoch: Ulid;
  generation: number;
  state: State;
  needs: NeedsYouItem[];
  brief: BriefData;
  local: LocalStatus;
  preferences: Preferences;
  notification_focus?: NotificationFocus;
  /** Agents seen through observe-only hooks (no skill); never questions. */
  presence?: PresenceRow[];
}

export interface PresenceRow {
  key: string;
  harness: string;
  state: "working" | "waiting" | "permission";
  /** When the current state began. */
  since: UtcTime;
  /** The folder it runs in. */
  folder: string;
  /** The linked project that folder belongs to, if any. */
  project?: ProjectSlug | null;
}

// ---------------------------------------------------------------------------
// Command requests and responses

export interface ApiErrorPayload {
  code: string;
  message: string;
  field?: string;
  details?: { field: string; message: string }[];
  hint: string;
  retryable: boolean;
}

export type ApiEnvelope<T> =
  | { v: 1; ok: true; data: T }
  | { v: 1; ok: false; error: ApiErrorPayload };

export interface GetSnapshotRequest {}

export interface GetProjectRequest {
  project: ProjectSlug;
}

export interface GetProjectData {
  project: ProjectState;
  graph: Graph;
}

export interface SearchRequest {
  text: string;
}

export interface SearchData {
  needs: NeedsYouItem[];
  projects: ProjectSummary[];
}

export interface PrepareAnswerRequest {
  ask_id: EventId;
  option_id: OptionId;
  /** The human's own words when option_id is `own`. */
  own_answer?: string;
  expected_receipt?: EventId;
  expected_decision_revision: Sha256;
}

/** Receipt fields except id/created_at/expected_finish, plus `preview:true`. */
export interface ReceiptPreview {
  preview: true;
  ask: EventId;
  node: NodePath;
  source: ReceiptSource;
  chose: DecisionOption;
  ruled_out: DecisionOption[];
  done_when: Criterion[];
  expected_eta?: EtaRange;
}

export interface PrepareAnswerData {
  token: string;
  expires_at: UtcTime;
  risk: Reversibility;
  label: string;
  receipt: ReceiptPreview;
  guard: Guard;
}

export interface AnswerDecisionRequest {
  request_id: Ulid;
  token: string;
  echo_label: string;
  done_when?: Criterion[];
  expected_eta?: EtaRange;
}

/** waiting: an agent is blocked on `wait` for it; connected: it reads the
 * answer at its next inbox or wait; away: no agent is connected. */
export type Listening = "waiting" | "connected" | "away";

export interface AnswerDecisionData {
  event_id: EventId;
  receipt: Receipt;
  awaiting_ack: true;
  released: NodePath[];
}

export interface ReviewDeliveryRequest {
  request_id: Ulid;
  delivery_id: EventId;
  expected_delivery_revision: Sha256;
  verdict: "accept" | "send_back";
  reason?: string;
  save_rule?: { rule_id: RuleId; text: string };
}

export interface ReviewDeliveryData {
  event_id: EventId;
  rule_event?: EventId;
  released: NodePath[];
}

export interface ResolveRescueRequest {
  request_id: Ulid;
  item_id: string;
  expected_rescue_revision: Sha256;
  action: "handoff" | "pause";
}

export interface ResolveRescueData {
  event_id: EventId;
}

export interface UpdateProjectRequest {
  request_id: Ulid;
  project: ProjectSlug;
  expected_fields: Record<string, Sha256>;
  patch: Record<string, unknown>;
}

export interface UpdateFeatureRequest {
  request_id: Ulid;
  project: ProjectSlug;
  node: NodePath;
  expected_fields: Record<string, Sha256>;
  expected_graph_revision?: Sha256;
  patch: Record<string, unknown>;
}

export interface EventResultData {
  event_id: EventId;
}

export interface SetPolicyRequest {
  request_id: Ulid;
  project: ProjectSlug;
  expected_revision: Sha256;
  policy: PolicySet;
}

export interface SetRuleRequest {
  request_id: Ulid;
  project: ProjectSlug;
  expected_revision: Sha256;
  rule: RuleSet;
}

export interface ReadReportRequest {
  project: ProjectSlug;
  blob: BlobRef;
}

export interface ReadReportData {
  mime: string;
  text?: string;
  media_token?: string;
  sha256: Sha256;
}

export interface ApproveCheckRequest {
  request_id: Ulid;
  expected_approval_revision: Sha256;
  profile: Record<string, unknown>;
  consent: true;
}

export interface ApproveCheckData {
  approval_id: string;
}

export interface RunCheckRequest {
  request_id: Ulid;
  project: ProjectSlug;
  delivery_id: EventId;
  criterion: CriterionId;
  approval_id: string;
  expected_delivery_revision: Sha256;
  expected_approval_revision: Sha256;
}

export interface RunCheckData {
  observation_id: EventId;
  result: CheckResult;
}

export interface SetPreferencesRequest {
  request_id: Ulid;
  expected_revision: Sha256;
  theme?: Theme;
  motion?: boolean;
  snooze?: { item_id: string; until: UtcTime };
  mark_looked?: boolean;
  /** Explicit local consent before the OS notification permission is requested. */
  notifications?: boolean;
  language?: "auto" | "en" | "zh-Hans" | "zh-Hant";
}

/** Close one waiting hand-off item for good on this computer. Main window and tray. */
export interface DismissNeedRequest {
  request_id: Ulid;
  item_id: string;
}

/** One local snooze entry; a time already passed ends it. Main window and tray. */
export interface SnoozeNeedRequest {
  request_id: Ulid;
  item_id: string;
  until: UtcTime;
}

export interface SetPreferencesData {
  preferences: Preferences;
}

export type SetupLocationKind = "icloud_drive" | "onedrive" | "local";

export interface SetupLocation {
  id: string;
  kind: SetupLocationKind;
  label: string;
  path: string;
  folder_name: string;
}

/** Read-only local configuration of this computer (get_local_config). */
export interface LocalConfig {
  vault?: string | null;
  data_dir: string;
  config_revision: Sha256;
  /** Canonical repository path -> project slug. */
  repos: Record<string, ProjectSlug>;
  codex_bin?: string | null;
  cli_path?: string | null;
  app_version: string;
}

export interface GetSetupLocationsData {
  locations: SetupLocation[];
}

export interface ConfigureVaultRequest {
  request_id: Ulid;
  expected_config_revision: Sha256;
  path: string;
  create: boolean;
  consent: true;
}

export interface ConfigureVaultData {
  vault_id: VaultId;
  host: HostId;
}

export interface InstallLocalRequest {
  request_id: Ulid;
  expected_plan_digest?: Sha256;
  kind: "path_link" | "hooks" | "observe_hooks";
  destination?: string;
  harness?: Harness;
  dry_run: boolean;
  consent: boolean;
}

export interface LinkRepoRequest {
  request_id: Ulid;
  repo: string;
  project: ProjectSlug;
}

export interface LinkRepoData {
  repo: string;
  project: ProjectSlug;
  expected_config_revision: Sha256;
}

export interface UnlinkRepoRequest {
  request_id: Ulid;
  /** The stored key: the canonical path shown by get_local_config. */
  repo: string;
  /** The project Settings showed for this folder; refused if it now links another. */
  expected_project?: ProjectSlug;
}

export interface UnlinkRepoData {
  repo: string;
  removed: boolean;
  /** The project the folder linked, or null if it was not linked. */
  project: ProjectSlug | null;
  /** Another folder on this computer still links that project. */
  project_still_linked: boolean;
  expected_config_revision: Sha256;
}

export interface InstallPlan {
  harness?: Harness;
  settings_path?: string;
  changes: { event: string; command: string; timeout_s: number }[];
  requires_consent: true;
  /** Re-read destination and echo this digest for a non-dry-run install. */
  plan_digest?: Sha256;
  already_installed?: boolean;
  /** PATH-link plan fields. */
  destination?: string;
  command?: string;
  cli_path?: string;
}

export interface InstallLocalData {
  plan: InstallPlan;
  installed: boolean;
}

export interface EmptyRequest {}

/** How the window paints its chrome on this machine. */
export interface WindowChrome {
  /** macOS: the traffic lights sit over the page; no native title row. */
  overlay_titlebar: boolean;
  /** `glass`: native Liquid Glass shows through the page's chrome regions. */
  material: "glass" | "solid";
}

/** The colour the native glass leans toward (sRGB, 0..1). */
export interface GlassTint {
  r: number;
  g: number;
  b: number;
  a: number;
}

/** One native glass rectangle, in CSS pixels from the webview's top left. */
export interface GlassRegion {
  x: number;
  y: number;
  width: number;
  height: number;
  radius: number;
  anchor: "fill" | "top_left" | "top_center" | "top_right";
}

export interface OpenPreviewRequest {
  request_id: Ulid;
  project: ProjectSlug;
  delivery_id: EventId;
  expected_delivery_revision: Sha256;
  demo_index: number;
  consent: true;
}

export interface OpenPreviewData {
  opened: boolean;
}

// ---------------------------------------------------------------------------
// Push events

export interface ChangedEvent {
  v: 1;
  epoch: Ulid;
  generation: number;
  event_set_digest: Sha256;
  as_of: UtcTime;
  projects: ProjectSlug[];
  pulses: {
    id: string;
    project: ProjectSlug;
    node: NodePath;
    host: HostId;
    event_ts: UtcTime;
  }[];
  batch: "local" | "sync" | "time" | "rebuild";
}

export interface HealthEvent {
  v: 1;
  health: Health;
  diagnostics: LocalDiagnostic[];
}

export interface UnblockEvent {
  v: 1;
  answer_id: EventId;
  project: ProjectSlug;
  from: string;
  released: NodePath[];
  phase: "answered" | "acked";
}

export type EventMap = {
  "agendar://changed": ChangedEvent;
  "agendar://health": HealthEvent;
  "agendar://unblock": UnblockEvent;
  "agendar://notification-focus": NotificationFocus;
};
