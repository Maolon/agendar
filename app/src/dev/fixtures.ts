/**
 * Development-only fixture state for the browser preview transport.
 *
 * This module is reachable only through the `import.meta.env.DEV` +
 * `VITE_AGENDAR_FIXTURES` guard in `src/api/client.ts`. `vite build` strips the
 * import, and a test asserts that a production bundle contains none of these
 * strings. The UI shows a "Fixture data" banner while this transport is active,
 * so it can never be mistaken for real Vault state.
 */

import type {
  BlobRef,
  DecisionCard,
  DecisionState,
  DeliveryState,
  FeatureState,
  Graph,
  GraphEdge,
  GraphNode,
  NeedsYouItem,
  PolicySet,
  ProjectState,
  Receipt,
  Snapshot,
} from "../api/types";

const EVALUATED_AT = "2026-02-10T15:00:00.000Z";
const ASK_SINCE = "2026-02-10T14:22:00.000Z";
const DEFAULT_AT = "2026-02-10T18:22:00.000Z";
const LAST_LOOKED = "2026-02-10T12:00:00.000Z";

/** Deterministic 64-char lowercase sha256-shaped token (fixtures only). */
export function sha(n: number): string {
  return n.toString(16).padStart(2, "0").repeat(32);
}

/** Deterministic 26-char uppercase ULID-shaped token (fixtures only). */
export function ulid(n: number): string {
  return `01J${String(n).padStart(23, "0")}`;
}

export const BALANCED: PolicySet = {
  preset: "balanced",
  cells: {
    easy_local: "auto",
    easy_wide: "assume",
    costly_local: "assume",
    costly_wide: "ask",
    irreversible_local: "ask",
    irreversible_wide: "ask",
  },
  domain_floor: {},
  budget: {
    max_speculative_features: 10,
    max_pending_slices: 2,
    max_demo_gap_s: 14400,
  },
  require_human_review: false,
  allow_timeouts: true,
};

export function feature(partial: Partial<FeatureState> & Pick<FeatureState, "node" | "title" | "slice">): FeatureState {
  return {
    demo_kind: "cli_transcript",
    priority: 1,
    paused: false,
    handoff: false,
    depends_on: [],
    field_sources: {},
    progress: "planned",
    gate: "open",
    basis: "firm",
    liveness: "quiet",
    assumptions: [],
    taints: [],
    waits_on: [],
    unblocks: [],
    field_revisions: {
      title: sha(0x81),
      slice: sha(0x82),
      demo_kind: sha(0x83),
      milestone: sha(0x84),
      deadline: sha(0x85),
      priority: sha(0x86),
      paused: sha(0x87),
      handoff: sha(0x88),
      depends_on: sha(0x89),
    },
    graph_revision: sha(0x77),
    ...partial,
  };
}

const IPC_CARD: DecisionCard = {
  why_now: "Two agents are blocked in their Stop hook until this is decided.",
  question: "How should answers reach idle agents?",
  reversibility: "easy",
  reach: "local",
  external: false,
  domain: "arch",
  options: [
    {
      id: "harness",
      label: "Harness adapters",
      consequence: "Hooks for Claude Code, queue for Codex",
      reversibility: "easy",
      external: false,
      eta: { min_s: 3600, max_s: 7200 },
      icon: "link",
    },
    {
      id: "inbox",
      label: "Inbox on next start",
      consequence: "No wake; applies on the next run",
      reversibility: "easy",
      external: false,
      eta: { min_s: 1800, max_s: 3600 },
      icon: "inbox",
    },
    {
      id: "daemon",
      label: "Local daemon",
      consequence: "Extra process to supervise",
      reversibility: "easy",
      external: false,
      cost: "Runs while Agendar is open",
      icon: "server",
    },
  ],
  recommended: {
    option: "harness",
    confidence: "high",
    because: "Hooks hold the session open until you answer.",
  },
  already_checked: [
    { text: "Stop hook can hold a session", claim: "seen" },
    { text: "Codex queue can message it", claim: "agent_says" },
  ],
  done_when: [
    { id: "wakes", text: "A finished session wakes and applies the receipt" },
  ],
  on_timeout: { kind: "proceed_as_assumption", after_s: 14400 },
};

const HISTORY_CARD: DecisionCard = {
  why_now: "Pane history is growing and the retention window is yours to set.",
  question: "How long should pane history be kept?",
  reversibility: "costly",
  reach: "local",
  external: false,
  domain: "ux",
  options: [
    {
      id: "week",
      label: "Keep 7 days of pane history",
      consequence: "Rework the storage layout once",
      reversibility: "costly",
      external: false,
      eta: { min_s: 7200, max_s: 10800 },
      icon: "hourglass",
    },
    {
      id: "day",
      label: "Keep 24 hours",
      consequence: "Less disk use, no rework",
      reversibility: "easy",
      external: false,
      eta: { min_s: 1800, max_s: 3600 },
      icon: "undo-2",
    },
  ],
  recommended: {
    option: "week",
    confidence: "high",
    because: "Matches how you resume panes after a restart.",
  },
  already_checked: [{ text: "Restore panes works after a restart", claim: "agent_says" }],
  done_when: [{ id: "restores", text: "A restart restores the last week of panes" }],
  on_timeout: { kind: "hold" },
};

const IPC_DECISION: DecisionState = {
  id: ulid(101),
  node: "core/ipc",
  card: IPC_CARD,
  mode: "ask",
  lifecycle: "open",
  answer_history: [],
  ack_history: [],
  default_at: DEFAULT_AT,
  built_on: [],
  pending_pivot: false,
  revision: sha(0x31),
};

const HISTORY_DECISION: DecisionState = {
  id: ulid(102),
  node: "panes/restore",
  card: HISTORY_CARD,
  mode: "ask",
  lifecycle: "open",
  answer_history: [],
  ack_history: [],
  built_on: [],
  pending_pivot: false,
  revision: sha(0x32),
};

const IPC_NEED: NeedsYouItem = {
  id: "decide:core-ipc",
  kind: "decide",
  project: "agendar",
  node: "core/ipc",
  source: IPC_DECISION.id,
  question: IPC_CARD.question,
  since: ASK_SINCE,
  stalled_agents: 2,
  waiting_minutes: 38,
  weight_x2: 20,
  score_x2: 29,
  reason: "2 agents waiting · 38m",
  unblocks: ["core/wake"],
  revision: sha(0x41),
};

const HISTORY_NEED: NeedsYouItem = {
  id: "decide:pane-history",
  kind: "decide",
  project: "panes",
  node: "panes/restore",
  source: HISTORY_DECISION.id,
  question: HISTORY_CARD.question,
  since: "2026-02-10T13:30:00.000Z",
  stalled_agents: 0,
  waiting_minutes: 90,
  weight_x2: 16,
  score_x2: 16,
  reason: "90m",
  unblocks: [],
  revision: sha(0x42),
};

const RESCUE_NEED: NeedsYouItem = {
  id: "rescue:lost:rule-fixtures",
  kind: "rescue",
  project: "style-guide",
  node: "fixtures/rule",
  source: ulid(103),
  question: "Agent stopped mid-task",
  since: "2026-02-10T14:38:00.000Z",
  stalled_agents: 0,
  waiting_minutes: 22,
  weight_x2: 18,
  score_x2: 25,
  reason: "Process exited while running Rule fixtures",
  unblocks: [],
  revision: sha(0x43),
};

const SESSION = ulid(201);
const SECOND_SESSION = ulid(204);
const THIRD_SESSION = ulid(205);
/** Session a fixture-simulated start attaches to (see fixtureTransport). */
export const FIXTURE_STARTED_SESSION = ulid(206);

/**
 * Development-only graph projection for one fixture project: features plus a
 * node for each pending decision and review,
 * Production never reaches this module.
 */
export function buildFixtureGraph(project: ProjectState): Graph {
  const nodes: GraphNode[] = [];
  const edges: GraphEdge[] = [];
  const needFor = (source: string): string | undefined =>
    project.needs_you.find((item) => item.source === source)?.id;
  const source = (node: string): string =>
    project.features[node]?.field_sources.depends_on ?? ulid(0x900);

  for (const [node, feature] of Object.entries(project.features)) {
    nodes.push({
      id: `feature:${node}`,
      kind: "feature",
      label: feature.title,
      feature: node,
      state: feature.progress,
      assumed: feature.basis !== "firm",
    });
  }
  for (const [node, feature] of Object.entries(project.features)) {
    for (const dependency of feature.depends_on) {
      if (!project.features[dependency]) continue;
      edges.push({ from: `feature:${dependency}`, to: `feature:${node}`, source_event: source(node) });
    }
  }
  for (const decision of Object.values(project.decisions)) {
    if (decision.lifecycle !== "open" && decision.lifecycle !== "defaulted") continue;
    const id = `decision:${decision.id}`;
    nodes.push({
      id,
      kind: "decision",
      label: decision.card.question,
      ...(needFor(decision.id) ? { need_id: needFor(decision.id)! } : {}),
      state: decision.lifecycle,
      assumed: decision.mode === "assume",
    });
    if (project.features[decision.node]) {
      edges.push({ from: id, to: `feature:${decision.node}`, source_event: decision.id });
    }
  }
  for (const delivery of Object.values(project.deliveries)) {
    if (delivery.review !== "pending" || delivery.superseded_by) continue;
    const id = `review:${delivery.id}`;
    nodes.push({
      id,
      kind: "review",
      label: delivery.name,
      ...(needFor(delivery.id) ? { need_id: needFor(delivery.id)! } : {}),
      state: delivery.review,
      assumed: false,
    });
    if (project.features[delivery.node]) {
      edges.push({ from: `feature:${delivery.node}`, to: id, source_event: delivery.id });
    }
    for (const [node, feature] of Object.entries(project.features)) {
      if (!feature.depends_on.includes(delivery.node)) continue;
      edges.push({ from: id, to: `feature:${node}`, source_event: delivery.id });
    }
  }
  return { nodes, edges };
}

/** Feature counts derived from the same map the views render. */
function counts(features: Record<string, FeatureState>): ProjectState["summary"]["feature_counts"] {
  const totals: ProjectState["summary"]["feature_counts"] = {};
  for (const feature of Object.values(features)) {
    totals[feature.progress] = (totals[feature.progress] ?? 0) + 1;
  }
  return totals;
}

/** A compact twelve-project fixture entry for projects without a long story. */
export function simpleProject(input: {
  slug: string;
  title: string;
  brief: string;
  goal: string;
  glyph: string;
  variance: ProjectState["summary"]["variance"];
  active_agents?: number;
  features?: Record<string, FeatureState>;
  calibration?: ProjectState["summary"]["calibration"];
  sessions?: ProjectState["sessions"];
  budget?: Partial<ProjectState["summary"]["budget"]>;
  needs?: NeedsYouItem[];
  decisions?: ProjectState["decisions"];
  receipts?: ProjectState["receipts"];
}): ProjectState {
  const features = input.features ?? {};
  const needsYou = input.needs ?? [];
  return {
    summary: {
      slug: input.slug,
      title: input.title,
      brief: input.brief,
      goal: input.goal,
      glyph: input.glyph,
      archived: false,
      variance: input.variance,
      active_agents: input.active_agents ?? 0,
      stalled_agents: needsYou.reduce((total, item) => total + item.stalled_agents, 0),
      needs_count: needsYou.length,
      feature_counts: counts(features),
      last_event: "2026-02-10T14:50:00.000Z",
      host_as_of: { "h-01h0000000000000000000000": "2026-02-10T14:59:00.000Z" },
      budget: {
        speculative_features: 0,
        pending_slices: 0,
        demo_gap_s: 0,
        blocked: false,
        reasons: [],
        ...input.budget,
      },
      calibration: input.calibration ?? [],
    },
    features,
    decisions: input.decisions ?? {},
    receipts: input.receipts ?? {},
    deliveries: {},
    rules: [],
    policy: { value: BALANCED, revision: sha(0x6a) },
    sessions: input.sessions ?? {},
    needs_you: needsYou,
    field_revisions: {
      title: sha(0x91),
      brief: sha(0x92),
      goal: sha(0x93),
      glyph: sha(0x94),
      archived: sha(0x95),
    },
  };
}

// --- Delivery review fixtures ----------------------------------------------

const ALIAS_ASK = ulid(104);
const ALIAS_RECEIPT = ulid(401);
const ALIAS_DELIVERY = ulid(402);
const ALIAS_REPORT = ulid(403);

const TRANSCRIPT_BLOB: BlobRef = {
  host: "h-01h0000000000000000000000",
  sha256: sha(0x81),
  ext: "txt",
  bytes: 214,
};

const SCREENSHOT_BLOB: BlobRef = {
  host: "h-01h0000000000000000000000",
  sha256: sha(0x82),
  ext: "png",
  bytes: 184_512,
};

const REPORT_BLOB: BlobRef = {
  host: "h-01h0000000000000000000000",
  sha256: sha(0x83),
  ext: "html",
  bytes: 96,
};

const ALIAS_RECEIPT_VALUE: Receipt = {
  id: ALIAS_RECEIPT,
  ask: ALIAS_ASK,
  node: "alias/search",
  source: "human",
  chose: {
    id: "alias",
    label: "Search by author alias",
    consequence: "A pen name resolves to the real author",
    reversibility: "easy",
    external: false,
  },
  ruled_out: [],
  done_when: [
    { id: "alias", text: "An alias finds the real author’s books" },
    { id: "title", text: "Title search still works" },
    { id: "speed", text: "Stays fast on a 100k-book library" },
  ],
  created_at: "2026-02-10T13:05:00.000Z",
};

const ALIAS_DELIVERY_VALUE: DeliveryState = {
  id: ALIAS_DELIVERY,
  node: "alias/search",
  attempt: ulid(304),
  receipt: ALIAS_RECEIPT,
  name: "search-v2",
  demos: [
    {
      kind: "cli_transcript",
      body: {
        blob: TRANSCRIPT_BLOB,
        // Proposed check: approved and run from the view only.
        check: {
          criterion: "speed",
          argv: [
            "agendar",
            "check-probe",
            "file_digest",
            "--path",
            "bench/speed.out",
            "--expected-sha256",
            "8f14e45fceea167a5a36dedd4bea2543b1f6b0e1a4d8e0b5c4c1e6e4a8f2a5d1",
          ],
          cwd: "repos/bookfinder",
          timeout_s: 30,
        },
      },
    },
    {
      kind: "screenshot",
      body: { blob: SCREENSHOT_BLOB, alt: "Alias search results in the terminal" },
    },
  ],
  criteria: [
    {
      criterion: { id: "alias", text: "An alias finds the real author’s books" },
      agent_result: "passed",
      seen_result: "passed",
      observation: ulid(305),
      provenance: "seen",
      conflict: false,
      observation_history: [ulid(305)],
      observation_frontier: [ulid(305)],
    },
    {
      criterion: { id: "title", text: "Title search still works" },
      agent_result: "passed",
      provenance: "agent_says",
      conflict: false,
      observation_history: [],
      observation_frontier: [],
    },
    {
      criterion: { id: "speed", text: "Stays fast on a 100k-book library" },
      agent_result: "not_proven",
      provenance: "not_proven_yet",
      conflict: false,
      observation_history: [],
      observation_frontier: [],
    },
  ],
  disclosure: {
    proven: ["Alias lookup returned the real author"],
    not_proven_yet: ["Large library speed"],
    faked_for_now: ["Alias list is a 12-entry sample"],
  },
  review: "pending",
  review_history: [],
  review_frontier: [],
  review_conflict: false,
  revision: sha(0x91),
};

const ALIAS_NEED: NeedsYouItem = {  id: "review:shelf-alias",
  kind: "review",
  project: "bookfinder",
  node: "alias/search",
  source: ALIAS_DELIVERY,
  question: "search-v2 is ready. Does it do what you asked?",
  since: "2026-02-10T14:40:00.000Z",
  stalled_agents: 0,
  waiting_minutes: 20,
  weight_x2: 2,
  score_x2: 0,
  reason: "Ready for review · 20m",
  unblocks: ["alias/export"],
  revision: sha(0x44),
};

const ALIAS_CARD: DecisionCard = {
  why_now: "Alias search was the only thing still open on this slice.",
  question: "Should author search follow pen names?",
  reversibility: "easy",
  reach: "local",
  external: false,
  domain: "code",
  options: [
    {
      id: "alias",
      label: "Search by author alias",
      consequence: "A pen name resolves to the real author",
      reversibility: "easy",
      external: false,
      eta: { min_s: 3600, max_s: 7200 },
      icon: "link",
    },
    {
      id: "title",
      label: "Title search only",
      consequence: "Aliases stay unsupported",
      reversibility: "easy",
      external: false,
      icon: "undo-2",
    },
  ],
  recommended: {
    option: "alias",
    confidence: "med",
    because: "Pen names are how the books are credited.",
  },
  already_checked: [{ text: "Book index has an author table", claim: "seen" }],
  done_when: [
    { id: "alias", text: "An alias finds the real author’s books" },
    { id: "title", text: "Title search still works" },
    { id: "speed", text: "Stays fast on a 100k-book library" },
  ],
  on_timeout: { kind: "hold" },
  report: REPORT_BLOB,
};

const ALIAS_DECISION: DecisionState = {
  id: ALIAS_ASK,
  node: "alias/search",
  card: ALIAS_CARD,
  mode: "ask",
  lifecycle: "answered",
  effective_receipt: ALIAS_RECEIPT,
  answer_history: [ALIAS_RECEIPT],
  ack_history: [ALIAS_REPORT],
  built_on: [],
  pending_pivot: false,
  revision: sha(0x34),
};

// --- Assumption fixture ----------------------------------------------------
// One live, unconfirmed assumption on screen-clips: the agent kept WebM and two
// features are built on that choice, so the assumptions view can be demoed.

const ASSUME_ASK = ulid(120);
const ASSUME_RECEIPT = ulid(121);

const ASSUME_CARD: DecisionCard = {
  why_now: "The recorder has to pick a container before the encoder lands.",
  question: "Keep WebM as the capture format?",
  reversibility: "costly",
  reach: "local",
  external: false,
  domain: "arch",
  options: [
    {
      id: "webm",
      label: "Keep WebM",
      consequence: "Nothing to redo",
      reversibility: "easy",
      external: false,
      eta: { min_s: 3600, max_s: 5400 },
      icon: "check",
    },
    {
      id: "mp4",
      label: "Switch to MP4",
      consequence: "The encoder is rewritten",
      reversibility: "costly",
      external: false,
      eta: { min_s: 7200, max_s: 10800 },
      icon: "refresh-cw",
    },
  ],
  recommended: { option: "webm", confidence: "high", because: "It plays in Safari." },
  already_checked: [{ text: "WebM plays in Safari 17", claim: "seen" }],
  done_when: [{ id: "plays", text: "Exports open in Safari and Chrome" }],
  on_timeout: { kind: "proceed_as_assumption", after_s: 14400 },
};

const ASSUME_DECISION: DecisionState = {
  id: ASSUME_ASK,
  node: "capture/record",
  card: ASSUME_CARD,
  mode: "assume",
  lifecycle: "defaulted",
  answer_history: [],
  ack_history: [],
  assumed_at: "2026-02-10T11:00:00.000Z",
  built_on: ["capture/export", "capture/record"],
  pending_pivot: false,
  revision: sha(0x35),
};

const ASSUME_RECEIPT_VALUE: Receipt = {
  id: ASSUME_RECEIPT,
  ask: ASSUME_ASK,
  node: "capture/record",
  source: "assumed",
  chose: ASSUME_CARD.options[0]!,
  ruled_out: [ASSUME_CARD.options[1]!],
  done_when: ASSUME_CARD.done_when,
  created_at: "2026-02-10T11:00:00.000Z",
};

/** A long project: most steps finished, so the overview folds older ones. */
const LEDGER_FEATURES: Record<string, FeatureState> = Object.fromEntries(
  Array.from({ length: 96 }, (_, i) => {
    const node = `log/step-${String(i + 1).padStart(2, "0")}`;
    const progress = i < 86 ? "verified" : i < 88 ? "done" : i === 88 ? "doing" : "planned";
    return [
      node,
      feature({
        node,
        title: `Ledger step ${i + 1}`,
        slice: `Slice ${Math.floor(i / 8) + 1}`,
        progress,
        liveness: progress === "doing" ? "live" : "quiet",
        depends_on: i % 8 ? [`log/step-${String(i).padStart(2, "0")}`] : [],
        ...(i < 89 ? { attempt: ulid(3000 + i) } : {}),
      }),
    ];
  }),
);

const PROJECTS: Record<string, ProjectState> = {
  agendar: {
    summary: {
      slug: "agendar",
      title: "Agendar",
      brief: "An agenda for your AI coding agents",
      goal: "Answer one question end to end on one Mac",
      glyph: "radar",
      archived: false,
      variance: "active",
      active_agents: 2,
      stalled_agents: 2,
      needs_count: 1,
      feature_counts: { verified: 2, doing: 1, planned: 1 },
      last_event: "2026-02-10T14:59:00.000Z",
      host_as_of: { "h-01h0000000000000000000000": "2026-02-10T14:59:00.000Z" },
      budget: {
        speculative_features: 9,
        pending_slices: 1,
        demo_gap_s: 0,
        blocked: false,
        reasons: ["9 features are built on unconfirmed choices"],
      },
      calibration: [],
    },
    features: {
      "core/ask": feature({
        node: "core/ask",
        title: "CLI ask",
        slice: "Slice 1 · Ask",
        progress: "verified",
        unblocks: ["core/ipc"],
      }),
      "core/ipc": feature({
        node: "core/ipc",
        title: "Wake method?",
        slice: "Slice 1 · Answer from the menu bar",
        gate: "awaiting_decision",
        depends_on: ["core/ask"],
        waits_on: [`decision:${IPC_DECISION.id}`],
        unblocks: ["core/wake"],
      }),
      "core/wake": feature({
        node: "core/wake",
        title: "Wake idle agents",
        slice: "Slice 1 · Wake",
        gate: "awaiting_upstream",
        depends_on: ["core/ipc"],
      }),
      "core/menubar": feature({
        node: "core/menubar",
        title: "Menu bar triage",
        slice: "Slice 1 · Tray",
        progress: "doing",
        liveness: "live",
        attempt: ulid(301),
        session: SESSION,
        last_pulse: ulid(302),
        eta: {
          agent_range: { min_s: 3600, max_s: 7200 },
          anchor: "2026-02-10T14:00:00.000Z",
          paused_s: 0,
          agent_finish: {
            earliest: "2026-02-10T15:00:00.000Z",
            latest: "2026-02-10T16:00:00.000Z",
          },
          late_by_s: 0,
          paused: false,
        },
      }),
      "core/glass": feature({
        node: "core/glass",
        title: "Window glass",
        slice: "Slice 1 · Tray",
        progress: "doing",
        liveness: "quiet",
        attempt: ulid(311),
        session: ulid(312),
        eta: {
          agent_range: { min_s: 1800, max_s: 3600 },
          anchor: "2026-02-10T11:20:00.000Z",
          paused_s: 0,
          agent_finish: {
            earliest: "2026-02-10T11:50:00.000Z",
            latest: "2026-02-10T12:20:00.000Z",
          },
          late_by_s: 9600,
          paused: false,
        },
      }),
    },
    decisions: { [IPC_DECISION.id]: IPC_DECISION },
    receipts: {},
    deliveries: {},
    rules: [
      {
        value: { rule_id: "no-sqlite-in-vault", text: "Never put SQLite in the Vault", enabled: true },
        revision: sha(0x51),
      },
    ],
    policy: { value: BALANCED, revision: sha(0x61) },
    sessions: {
      [SESSION]: {
        id: SESSION,
        host: "h-01h0000000000000000000000",
        harness: "claude_code",
        last_event: ulid(302),
        last_event_ts: "2026-02-10T14:59:00.000Z",
        current_node: "core/menubar",
        liveness: "live",
        stalled_on: [IPC_DECISION.id],
      },
      [ulid(312)]: {
        id: ulid(312),
        host: "h-01h0000000000000000000000",
        harness: "claude_code",
        last_event: ulid(313),
        last_event_ts: "2026-02-10T11:41:00.000Z",
        current_node: "core/glass",
        liveness: "quiet",
        stalled_on: [],
      },
    },
    needs_you: [IPC_NEED],
    field_revisions: { goal: sha(0x7d) },
  },
  panes: {
    summary: {
      slug: "panes",
      title: "Panes",
      brief: "Terminal multiplexer for coding agents",
      goal: "Restore panes after a restart",
      glyph: "columns-2",
      archived: false,
      variance: "active",
      active_agents: 1,
      stalled_agents: 0,
      needs_count: 1,
      feature_counts: { verified: 1, doing: 1, planned: 1 },
      last_event: "2026-02-10T14:48:00.000Z",
      host_as_of: { "h-01h0000000000000000000000": "2026-02-10T14:59:00.000Z" },
      budget: {
        speculative_features: 0,
        pending_slices: 0,
        demo_gap_s: 0,
        blocked: false,
        reasons: [],
      },
      calibration: [],
    },
    features: {
      "panes/layout": feature({
        node: "panes/layout",
        title: "Pane layout",
        slice: "Slice 2 · Layout",
        progress: "verified",
        unblocks: ["panes/restore"],
      }),
      "panes/restore": feature({
        node: "panes/restore",
        title: "Restore panes",
        slice: "Slice 2 · Restore",
        progress: "doing",
        gate: "awaiting_decision",
        liveness: "live",
        depends_on: ["panes/layout"],
        waits_on: [`decision:${HISTORY_DECISION.id}`],
        attempt: ulid(303),
        session: ulid(202),
        eta: {
          agent_range: { min_s: 7200, max_s: 10800 },
          anchor: "2026-02-10T13:00:00.000Z",
          paused_s: 0,
          agent_finish: {
            earliest: "2026-02-10T15:00:00.000Z",
            latest: "2026-02-10T16:00:00.000Z",
          },
          late_by_s: 2400,
          paused: false,
        },
      }),
      "panes/search": feature({
        node: "panes/search",
        title: "Session search",
        slice: "Slice 3 · Search",
        gate: "awaiting_upstream",
        depends_on: ["panes/layout"],
      }),
    },
    decisions: { [HISTORY_DECISION.id]: HISTORY_DECISION },
    receipts: {},
    deliveries: {},
    rules: [],
    policy: { value: BALANCED, revision: sha(0x62) },
    sessions: {},
    needs_you: [HISTORY_NEED],
    field_revisions: {},
  },
  "style-guide": {
    summary: {
      slug: "style-guide",
      title: "Style guide",
      brief: "Rule modeling toolkit",
      goal: "Validate rule fixtures",
      glyph: "box",
      archived: false,
      variance: "stopped",
      active_agents: 0,
      stalled_agents: 0,
      needs_count: 1,
      feature_counts: { verified: 1, planned: 1 },
      last_event: "2026-02-10T14:38:00.000Z",
      host_as_of: { "h-01h0000000000000000000000": "2026-02-10T14:59:00.000Z" },
      budget: {
        speculative_features: 0,
        pending_slices: 0,
        demo_gap_s: 0,
        blocked: false,
        reasons: [],
      },
      calibration: [],
    },
    features: {
      "lint/gate": feature({
        node: "lint/gate",
        title: "Lint gate",
        slice: "Slice 2 · Lint",
        progress: "verified",
        unblocks: ["fixtures/rule"],
      }),
      "fixtures/rule": feature({
        node: "fixtures/rule",
        title: "Rule fixtures",
        slice: "Slice 2 · Rule fixtures",
        gate: "awaiting_decision",
        liveness: "lost",
        depends_on: ["lint/gate"],
      }),
    },
    decisions: {},
    receipts: {},
    deliveries: {},
    rules: [],
    policy: { value: BALANCED, revision: sha(0x63) },
    sessions: {},
    needs_you: [RESCUE_NEED],
    field_revisions: {},
  },
  "bookfinder": {
    summary: {
      slug: "bookfinder",
      title: "bookfinder",
      brief: "Search and download books from the terminal",
      goal: "Find books by author alias",
      glyph: "library",
      archived: false,
      variance: "active",
      active_agents: 1,
      stalled_agents: 0,
      needs_count: 1,
      feature_counts: { verified: 3, done: 1, planned: 1 },
      last_event: "2026-02-10T14:52:00.000Z",
      host_as_of: { "h-01h0000000000000000000000": "2026-02-10T14:59:00.000Z" },
      budget: {
        speculative_features: 0,
        pending_slices: 1,
        demo_gap_s: 0,
        blocked: false,
        reasons: [],
      },
      calibration: [],
    },
    features: {
      "alias/index": feature({
        node: "alias/index",
        title: "Alias index",
        slice: "Slice 2 · Alias index",
        progress: "verified",
        unblocks: ["alias/search"],
      }),
      "alias/search": feature({
        node: "alias/search",
        title: "Alias search",
        slice: "Slice 2 · Search by author alias",
        progress: "done",
        gate: "open",
        depends_on: ["alias/index"],
        unblocks: ["alias/export"],
        attempt: ulid(304),
        session: ulid(203),
      }),
      "alias/export": feature({
        node: "alias/export",
        title: "Export results",
        slice: "Slice 3 · Export",
        gate: "awaiting_upstream",
        depends_on: ["alias/search"],
      }),
    },
    decisions: { [ALIAS_ASK]: ALIAS_DECISION },
    receipts: { [ALIAS_RECEIPT]: ALIAS_RECEIPT_VALUE },
    deliveries: { [ALIAS_DELIVERY]: ALIAS_DELIVERY_VALUE },
    reports: [
      {
        id: ulid(951),
        node: "alias/search",
        title: "Search ranking compared",
        summary:
          "Prefix match beats fuzzy for short aliases; fuzzy only helps past 6 characters.\nRecommend prefix first, fuzzy as a fallback.",
        report: { host: "h-01h0000000000000000000000", sha256: sha(0x77), bytes: 4200, ext: "html" },
        ts: "2026-02-10T14:40:00.000Z",
        session: ulid(203),
        harness: "codex",
      },
      {
        id: ulid(952),
        title: "Download limit audit",
        summary: "No path exceeds the daily limit; the retry loop counts each attempt once.",
        report: { host: "h-01h0000000000000000000000", sha256: sha(0x78), bytes: 1800, ext: "md" },
        ts: "2026-02-10T13:05:00.000Z",
        session: ulid(203),
        harness: "codex",
      },
    ],
    rules: [
      {
        value: { rule_id: "respect-daily-limit", text: "Respect the daily download limit", enabled: true },
        revision: sha(0x52),
      },
    ],
    policy: { value: BALANCED, revision: sha(0x65) },
    sessions: {
      [ulid(203)]: {
        id: ulid(203),
        host: "h-01h0000000000000000000000",
        harness: "codex",
        last_event: ulid(306),
        last_event_ts: "2026-02-10T14:52:00.000Z",
        current_node: "alias/search",
        liveness: "quiet",
        stalled_on: [],
      },
    },
    needs_you: [ALIAS_NEED],
    field_revisions: {},
  },
  "help-site": {
    summary: {
      slug: "help-site",
      title: "Help site",
      brief: "Documentation site",
      goal: "No active goal",
      glyph: "file-text",
      archived: false,
      variance: "quiet",
      active_agents: 0,
      stalled_agents: 0,
      needs_count: 0,
      feature_counts: {},
      host_as_of: {},
      budget: {
        speculative_features: 0,
        pending_slices: 0,
        demo_gap_s: 0,
        blocked: false,
        reasons: [],
      },
      calibration: [],
    },
    features: {},
    decisions: {},
    receipts: {},
    deliveries: {},
    rules: [],
    policy: { value: BALANCED, revision: sha(0x64) },
    sessions: {},
    needs_you: [],
    field_revisions: {},
  },
  "screen-clips": simpleProject({
    slug: "screen-clips",
    title: "Screen clips",
    brief: "Browser control for logged-in sessions",
    goal: "Record a rendered page to WebM",
    glyph: "cloud",
    variance: "rework",
    active_agents: 1,
    features: {
      "capture/session": feature({
        node: "capture/session",
        title: "Attach to a session",
        slice: "Slice 3 · Session",
        progress: "verified",
        unblocks: ["capture/record"],
      }),
      "capture/record": feature({
        node: "capture/record",
        title: "Record the page",
        slice: "Slice 4 · Region recording",
        progress: "doing",
        gate: "open",
        liveness: "live",
        basis: "assumed",
        assumptions: [ASSUME_RECEIPT],
        depends_on: ["capture/session"],
        session: SECOND_SESSION,
        attempt: ulid(311),
        eta: {
          agent_range: { min_s: 3600, max_s: 5400 },
          anchor: "2026-02-10T14:30:00.000Z",
          paused_s: 0,
          agent_finish: {
            earliest: "2026-02-10T15:30:00.000Z",
            latest: "2026-02-10T16:00:00.000Z",
          },
          late_by_s: 0,
          paused: false,
        },
      }),
      "capture/export": feature({
        node: "capture/export",
        title: "Export & share",
        slice: "Slice 5 · Export",
        progress: "planned",
        gate: "awaiting_upstream",
        basis: "assumed",
        assumptions: [ASSUME_RECEIPT],
        depends_on: ["capture/record"],
        field_sources: { depends_on: ulid(315) },
      }),
    },
    decisions: { [ASSUME_ASK]: ASSUME_DECISION },
    receipts: { [ASSUME_RECEIPT]: ASSUME_RECEIPT_VALUE },
    sessions: {
      [SECOND_SESSION]: {
        id: SECOND_SESSION,
        host: "h-01h0000000000000000000000",
        harness: "pi",
        last_event: ulid(312),
        last_event_ts: "2026-02-10T14:57:00.000Z",
        current_node: "capture/record",
        liveness: "live",
        stalled_on: [],
      },
    },
  }),
  meridian: simpleProject({
    slug: "meridian",
    title: "Meridian",
    brief: "Model builder from project evidence",
    goal: "Build M1–M7 from a code tree",
    glyph: "circle-dot",
    variance: "active",
    active_agents: 1,
    features: {
      "model/objects": feature({
        node: "model/objects",
        title: "Object inventory",
        slice: "Slice 2 · Objects",
        progress: "verified",
        unblocks: ["model/relations"],
      }),
      "model/relations": feature({
        node: "model/relations",
        title: "Relation edges",
        slice: "Slice 3 · Relations",
        progress: "verified",
        depends_on: ["model/objects"],
        unblocks: ["model/encode"],
      }),
      "model/encode": feature({
        node: "model/encode",
        title: "Encode the model",
        slice: "Slice 4 · Encoder",
        progress: "doing",
        gate: "open",
        liveness: "live",
        priority: 0,
        depends_on: ["model/relations"],
        session: THIRD_SESSION,
        attempt: ulid(313),
        deadline: "2026-02-12T17:00:00.000Z",
        field_sources: { deadline: ulid(314), priority: ulid(314), title: ulid(314) },
        field_revisions: {
          deadline: sha(0x7a),
          priority: sha(0x7b),
          title: sha(0x7c),
        },
        eta: {
          agent_range: { min_s: 5400, max_s: 7200 },
          anchor: "2026-02-10T14:00:00.000Z",
          paused_s: 0,
          agent_finish: {
            earliest: "2026-02-10T15:30:00.000Z",
            latest: "2026-02-10T16:00:00.000Z",
          },
          likely_finish: {
            earliest: "2026-02-10T15:45:00.000Z",
            latest: "2026-02-10T16:30:00.000Z",
          },
          ratio_milli: 1500,
          late_by_s: 0,
          paused: false,
        },
      }),
    },
    calibration: [{ harness: "claude_code", samples: 6, ratio_milli: 1500 }],
    sessions: {
      [THIRD_SESSION]: {
        id: THIRD_SESSION,
        host: "h-01h0000000000000000000000",
        harness: "claude_code",
        last_event: ulid(314),
        last_event_ts: "2026-02-10T14:56:00.000Z",
        current_node: "model/encode",
        liveness: "live",
        stalled_on: [],
      },
    },
  }),
  "relay-box": simpleProject({
    slug: "relay-box",
    title: "Relay box",
    brief: "Wake idle agent sessions",
    goal: "Wake a finished session within a minute",
    glyph: "radio",
    variance: "active",
    features: {
      "relay/session": feature({
        node: "relay/session",
        title: "Relay session",
        slice: "Slice 2 · Relay",
        progress: "doing",
        gate: "open",
        liveness: "quiet",
        paused: true,
        priority: 2,
      }),
      "relay/wake": feature({
        node: "relay/wake",
        title: "Wake the session",
        slice: "Slice 3 · Wake",
        gate: "awaiting_upstream",
        depends_on: ["relay/session"],
      }),
    },
  }),
  "toolbelt": simpleProject({
    slug: "toolbelt",
    title: "Toolbelt",
    brief: "Hooks and helpers for coding agents",
    goal: "Install hooks without guessing",
    glyph: "box",
    variance: "no_demo",
    features: {
      "kit/install": feature({
        node: "kit/install",
        title: "Hook installer",
        slice: "Slice 2 · Installer",
        progress: "verified",
        unblocks: ["kit/fixtures"],
      }),
      "kit/fixtures": feature({
        node: "kit/fixtures",
        title: "Hook fixtures",
        slice: "Slice 3 · Fixtures",
        progress: "doing",
        gate: "open",
        liveness: "quiet",
        depends_on: ["kit/install"],
      }),
    },
  }),
  notes: simpleProject({
    slug: "notes",
    title: "Notes",
    brief: "Interview notes pipeline",
    goal: "Turn transcripts into internal notes",
    glyph: "file-text",
    variance: "quiet",
  }),
  site: simpleProject({
    slug: "site",
    title: "Site",
    brief: "Static marketing site",
    goal: "Publish the release page",
    glyph: "server",
    variance: "quiet",
  }),
  ledger: simpleProject({
    slug: "ledger",
    title: "Ledger",
    brief: "Decision ledger tools",
    goal: "Keep the decision log readable",
    glyph: "library",
    variance: "quiet",
    active_agents: 1,
    features: LEDGER_FEATURES,
  }),
};

export function buildFixtureSnapshot(): Snapshot {
  return structuredClone({
    v: 1,
    epoch: ulid(900),
    navigation_epoch: ulid(901),
    generation: 7,
    state: {
      projects: PROJECTS,
      diagnostics: [],
      event_set_digest: sha(0x99),
      evaluated_at: EVALUATED_AT,
    },
    needs: [IPC_NEED, ALIAS_NEED, HISTORY_NEED, RESCUE_NEED],
    brief: {
      delivered: [
        {
          id: "delivered:alias",
          text: "bookfinder: search finds books by author alias",
          detail: "Needs your review",
          target: { kind: "need", id: ALIAS_NEED.id },
          source_ids: [ALIAS_DELIVERY],
        },
      ],
      coming_up: [
        {
          id: "coming:menubar",
          text: "agendar: Menu bar triage likely done ~16:00",
          detail: "Agent said 15:00",
          target: { kind: "project", id: "agendar" },
          source_ids: [ulid(302)],
        },
        {
          id: "coming:encode",
          text: "meridian: Encode the model likely done ~15:45",
          detail: "Agent said ~15:30; calibrated 1.5×",
          target: { kind: "project", id: "meridian" },
          source_ids: [ulid(313)],
        },
      ],
      needs_you: [
        {
          id: "needs:ipc",
          text: "agendar: 1 decision, 2 agents waiting",
          target: { kind: "need", id: IPC_NEED.id },
          source_ids: [IPC_DECISION.id],
        },
      ],
      if_you_wait: [
        {
          id: "wait:ipc",
          text: "agendar: Wake idle agents and 1 more step stay blocked",
          target: { kind: "project", id: "agendar" },
          source_ids: [IPC_DECISION.id],
        },
      ],
      by_default: [
        {
          id: "default:ipc",
          text: "agendar goes with Harness adapters at 18:22 if nobody answers",
          target: { kind: "need", id: IPC_NEED.id },
          source_ids: [IPC_DECISION.id],
        },
      ],
      at_risk: [
        {
          id: "risk:panes",
          text: "panes is 40m past the agent's estimate",
          detail: "Last pulse 12m ago",
          target: { kind: "project", id: "panes" },
          source_ids: [ulid(303)],
        },
        {
          id: "risk:budget",
          text: "agendar: 9 of 10 features are built on unconfirmed choices",
          detail: "One more speculation would exceed the budget",
          target: { kind: "project", id: "agendar" },
          source_ids: [IPC_DECISION.id],
        },
      ],
    },
    local: {
      host: "h-01h0000000000000000000000",
      received_at: "2026-02-10T15:00:00.000Z",
      health: "healthy",
      diagnostics: [],
      capabilities: [
        {
          harness: "claude_code",
          running: true,
          ended_turn: true,
          new_session: true,
          queue_supported: false,
          automatic_wake: "available",
          detail: "Claude Code hooks registered",
        },
      ],
      notifications: "granted",
      notification_actions: { availability: "available" },
    },
    preferences: {
      theme: "system",
      motion: true,
      snoozed: {},
      last_looked_at: LAST_LOOKED,
      last_looked_ids: [],
      revision: sha(0x71),
    },
  } satisfies Snapshot);
}
