/**
 * Labelled DTO fixtures and a fake typed client. All IPC is mocked here and
 * only here; production code always talks through `src/api/client.ts`.
 */

import { ownOption } from "../api/format";
import { ApiError, type AgendarClient, type Transport } from "../api/client";
import { cardRisk } from "../api/format";
import { AnswerAction } from "../state/answer";
import type { ProjectProps } from "../views/Project";
import type {
  GlassRegion,
  GlassTint,
  WindowChrome,
  AnswerDecisionData,
  AnswerDecisionRequest,
  ApproveCheckData,
  ApproveCheckRequest,
  BlobRef,
  ChangedEvent,
  CheckResult,
  ConfigureVaultData,
  ConfigureVaultRequest,
  DecisionCard,
  DecisionState,
  DeliveryState,
  DemoCheck,
  EventMap,
  GetSetupLocationsData,
  LocalConfig,
  StorageUsage,
  UsageNumbers,
  Graph,
  InstallLocalData,
  InstallLocalRequest,
  InstallPlan,
  LinkRepoData,
  UnlinkRepoData,
  TestNoticeData,
  OpenReportData,
  CheckUpdatesData,
  OpenReleaseData,
  OpenReportRequest,
  Binding,
  GetBindingsData,
  SetBindingRequest,
  SetBindingData,
  UnlinkRepoRequest,
  NeedsYouItem,
  Preferences,
  PrepareAnswerData,
  PrepareAnswerRequest,
  ProjectState,
  ReadReportData,
  ReadReportRequest,
  Receipt,
  Reversibility,
  ReviewDeliveryData,
  ReviewDeliveryRequest,
  RunCheckData,
  RunCheckRequest,
  SetPreferencesData,
  SetPreferencesRequest,
  SnoozeNeedData,
  SnoozeNeedRequest,
  DismissNeedRequest,
  SetupLocation,
  Snapshot,
} from "../api/types";
import type { UnlistenFn } from "@tauri-apps/api/event";

export function sha(n: number): string {
  return n.toString(16).padStart(2, "0").repeat(32);
}

/** Empty graph by default; a test publishes nodes with `client.graph = …`. */
export function makeGraph(overrides: Partial<Graph> = {}): Graph {
  return { nodes: [], edges: [], ...overrides };
}

/** Props one project page needs, so tests set only what they assert. */
export function projectProps(
  project: ProjectState,
  client: FakeClient,
  overrides: Partial<ProjectProps> = {},
): ProjectProps {
  return {
    project,
    anchor: EVALUATED_AT,
    canMutate: true,
    agentsOpen: false,
    onToggleAgents: () => undefined,
    action: new AnswerAction(client, {
      current: { onAnswered: () => undefined, onRefresh: () => undefined },
    }),
    motion: false,
    pulses: [],
    pulseToken: 0,
    pulseAges: {},
    unblock: null,
    onUnblockPlayed: () => undefined,
    ...overrides,
  };
}

export function ulid(n: number): string {
  return `01J${String(n).padStart(23, "0")}`;
}

export const EVALUATED_AT = "2026-02-10T15:00:00.000Z";

const BASE_OPTIONS = [
  {
    id: "one",
    label: "Use harness adapters",
    consequence: "Hooks hold the session",
    reversibility: "easy" as const,
    external: false,
    eta: { min_s: 3600, max_s: 7200 },
    icon: "link",
  },
  {
    id: "two",
    label: "Use inbox on next start",
    consequence: "Applies on the next run",
    reversibility: "easy" as const,
    external: false,
    icon: "inbox",
  },
  {
    id: "three",
    label: "Use a local daemon",
    consequence: "Extra process to supervise",
    reversibility: "easy" as const,
    external: false,
    icon: "server",
  },
];

export function easyCard(overrides: Partial<DecisionCard> = {}): DecisionCard {
  return {
    why_now: "Agents are blocked until this is decided.",
    question: "How should answers reach idle agents?",
    reversibility: "easy",
    reach: "local",
    external: false,
    domain: "arch",
    options: BASE_OPTIONS,
    recommended: { option: "one", confidence: "high", because: "It holds the session open." },
    already_checked: [
      { text: "Stop hook can hold a session", claim: "seen" },
      { text: "Queue can message it", claim: "agent_says" },
    ],
    done_when: [{ id: "wakes", text: "A finished session applies the receipt" }],
    on_timeout: { kind: "proceed_as_assumption", after_s: 14400 },
    ...overrides,
  };
}

export function costlyCard(overrides: Partial<DecisionCard> = {}): DecisionCard {
  return easyCard({
    question: "How long should pane history be kept?",
    reversibility: "costly",
    options: [
      {
        id: "week",
        label: "Keep 7 days of pane history",
        consequence: "Rework the storage layout",
        reversibility: "costly",
        external: false,
        eta: { min_s: 7200, max_s: 10800 },
        icon: "hourglass",
      },
      {
        id: "day",
        label: "Keep 24 hours",
        consequence: "Less disk use",
        reversibility: "easy",
        external: false,
        icon: "undo-2",
      },
    ],
    recommended: { option: "week", confidence: "high", because: "Matches how you resume panes." },
    on_timeout: { kind: "hold" },
    ...overrides,
  });
}

export function irreversibleCard(overrides: Partial<DecisionCard> = {}): DecisionCard {
  return easyCard({
    question: "Deploy the new schema now?",
    reversibility: "irreversible",
    options: [
      {
        id: "deploy",
        label: "Deploy now",
        consequence: "Existing readers break",
        reversibility: "irreversible",
        external: true,
        icon: "lock",
      },
      {
        id: "wait",
        label: "Wait until Friday",
        consequence: "No rework",
        reversibility: "easy",
        external: false,
        icon: "undo-2",
      },
    ],
    recommended: { option: "wait", confidence: "low", because: "Safe but slow." },
    on_timeout: { kind: "hold" },
    ...overrides,
  });
}

export const ASK_ONE = ulid(101);
export const ASK_TWO = ulid(102);

export function makeDecision(
  id: string,
  card: DecisionCard,
  overrides: Partial<DecisionState> = {},
): DecisionState {
  return {
    id,
    node: "core/one",
    card,
    mode: "ask",
    lifecycle: "open",
    answer_history: [],
    ack_history: [],
    default_at: "2026-02-10T18:22:00.000Z",
    built_on: [],
    pending_pivot: false,
    revision: sha(0x31),
    ...overrides,
  };
}

export function makeNeed(overrides: Partial<NeedsYouItem> & Pick<NeedsYouItem, "id">): NeedsYouItem {
  return {
    kind: "decide",
    project: "alpha",
    node: "core/one",
    source: ASK_ONE,
    question: "How should answers reach idle agents?",
    since: "2026-02-10T14:22:00.000Z",
    stalled_agents: 2,
    waiting_minutes: 38,
    weight_x2: 20,
    score_x2: 29,
    reason: "2 agents waiting",
    unblocks: ["core/two"],
    revision: sha(0x41),
    ...overrides,
  };
}

export function makeProject(
  slug: string,
  overrides: Partial<ProjectState> = {},
): ProjectState {  return {
    summary: {
      slug,
      title: slug,
      brief: "A fixture project",
      goal: "Ship the fixture",
      glyph: "radar",
      archived: false,
      variance: "active",
      active_agents: 1,
      stalled_agents: 2,
      needs_count: 1,
      feature_counts: { verified: 1, doing: 1 },
      last_event: "2026-02-10T14:59:00.000Z",
      host_as_of: { "h-fixture": EVALUATED_AT },
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
      "core/one": {
        node: "core/one",
        title: "CLI ask",
        slice: "Slice 1 · Ask",
        demo_kind: "cli_transcript",
        priority: 1,
        paused: false,
        handoff: false,
        depends_on: [],
        field_sources: {},
        progress: "doing",
        gate: "awaiting_decision",
        basis: "firm",
        liveness: "live",
        assumptions: [],
        taints: [],
        waits_on: [],
        unblocks: ["core/two"],
        field_revisions: {},
        graph_revision: sha(0x55),
      },
    },
    decisions: {},
    receipts: {},
    deliveries: {},
    rules: [
      {
        value: { rule_id: "no-shell", text: "Use explicit argv", enabled: true },
        revision: sha(0x51),
      },
    ],
    policy: {
      value: {
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
      },
      revision: sha(0x61),
    },
    sessions: {},
    needs_you: [],
    field_revisions: {},
    ...overrides,
  };
}

// --- Delivery review fixtures ------------------------------------------------

export function textBlob(sha256 = sha(0x81), ext = "txt"): BlobRef {
  return { host: "h-fixture", sha256, ext, bytes: 64 };
}

export function imageBlob(sha256 = sha(0x82)): BlobRef {
  return { host: "h-fixture", sha256, ext: "png", bytes: 2048 };
}

export const DELIVERY_ID = ulid(600);
export const DELIVERY_RECEIPT = ulid(602);
export const DELIVERY_ASK = ulid(603);
export const DELIVERY_REPORT = sha(0x83);

export function makeDelivery(overrides: Partial<DeliveryState> = {}): DeliveryState {
  return {
    id: DELIVERY_ID,
    node: "core/one",
    attempt: ulid(601),
    receipt: DELIVERY_RECEIPT,
    name: "search-v2",
    demos: [
      {
        kind: "cli_transcript",
        body: {
          blob: textBlob(),
          // The exact profile the cross-language approval-revision golden uses
          // (`the_absent_approval_revision_is_the_documented_hash` in the core
          // test suite asserts the same two hashes).
          check: {
            criterion: "title",
            argv: [
              "/opt/agendar/bin/agendar-check",
              "text_equals",
              "--actual",
              "one two.txt",
            ],
            cwd: "repos/alpha",
            timeout_s: 30,
          },
        },
      },
      { kind: "screenshot", body: { blob: imageBlob(), alt: "Alias results" } },
    ],
    criteria: [
      {
        criterion: { id: "alias", text: "An alias finds the real author" },
        agent_result: "passed",
        seen_result: "passed",
        observation: ulid(604),
        provenance: "seen",
        conflict: false,
        observation_history: [ulid(604)],
        observation_frontier: [ulid(604)],
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
        criterion: { id: "speed", text: "Fast on a large library" },
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
      faked_for_now: ["Alias list is a sample"],
    },
    review: "pending",
    review_history: [],
    review_frontier: [],
    review_conflict: false,
    revision: sha(0x92),
    ...overrides,
  };
}

/** The same delivery with a different proposed check profile. */
export function makeCheckDelivery(check: Partial<DemoCheck>): DeliveryState {
  const base = makeDelivery();
  return {
    ...base,
    demos: base.demos.map((demo, index) =>
      index === 0
        ? {
            kind: demo.kind,
            body: {
              ...demo.body,
              check: { ...(demo.body.check as DemoCheck), ...check },
            },
          }
        : demo,
    ),
  };
}

export function makeReviewNeed(
  overrides: Partial<NeedsYouItem> & Pick<NeedsYouItem, "id"> = { id: "review-one" },
): NeedsYouItem {
  return {
    kind: "review",
    project: "alpha",
    node: "core/one",
    source: DELIVERY_ID,
    question: "search-v2 is ready. Does it do what you asked?",
    since: "2026-02-10T14:40:00.000Z",
    stalled_agents: 0,
    waiting_minutes: 20,
    weight_x2: 2,
    score_x2: 40,
    reason: "Ready for review · 20m",
    unblocks: ["core/two"],
    revision: sha(0x44),
    ...overrides,
  };
}

/**
 * A project whose delivery still waits for review: the receipt that produced
 * it, the answered ask that carries the report, and the review need.
 */
export function makeDeliveryProject(): {
  project: ProjectState;
  delivery: DeliveryState;
  need: NeedsYouItem;
} {
  const delivery = makeDelivery();
  const ask = makeDecision(DELIVERY_ASK, easyCard({ report: textBlob(sha(0x84), "html") }), {
    lifecycle: "answered",
    effective_receipt: DELIVERY_RECEIPT,
  });
  const receipt: Receipt = {
    id: DELIVERY_RECEIPT,
    ask: DELIVERY_ASK,
    node: "core/one",
    source: "human",
    chose: easyCard().options[0]!,
    ruled_out: [],
    done_when: delivery.criteria.map((entry) => entry.criterion),
    created_at: EVALUATED_AT,
  };
  const need = makeReviewNeed();
  const project = makeProject("alpha", {
    decisions: { [DELIVERY_ASK]: ask },
    receipts: { [DELIVERY_RECEIPT]: receipt },
    deliveries: { [delivery.id]: delivery },
    needs_you: [need],
  });
  return { project, delivery, need };
}

export function basePreferences(): Preferences { return {
    theme: "system",
    motion: true,
    last_looked_at: "2026-02-10T12:00:00.000Z",
    last_looked_ids: [],
    revision: sha(0x71),
  };
}

export interface SnapshotOptions {
  firstCard?: DecisionCard;
  secondCard?: DecisionCard;
  withRescue?: boolean;
}

export function makeSnapshot(options: SnapshotOptions = {}): Snapshot {  const first = makeDecision(ASK_ONE, options.firstCard ?? easyCard());
  const second = makeDecision(ASK_TWO, options.secondCard ?? costlyCard(), {
    node: "core/two",
    revision: sha(0x32),
  });
  const alpha = makeProject("alpha");
  alpha.decisions = { [first.id]: first };
  const beta = makeProject("beta", {
    summary: {
      ...makeProject("beta").summary,
      title: "beta",
      needs_count: 1,
      variance: "active",
    },
  });
  beta.decisions = { [second.id]: second };
  const gamma = makeProject("gamma", {
    summary: {
      ...makeProject("gamma").summary,
      variance: "stopped",
      active_agents: 0,
      needs_count: options.withRescue === false ? 0 : 1,
      feature_counts: { verified: 1, planned: 1 },
    },
    features: {
      "core/one": {
        ...makeProject("gamma").features["core/one"]!,
        liveness: "lost",
        progress: "planned",
      },
    },
  });
  const delta = makeProject("delta", {
    summary: {
      ...makeProject("delta").summary,
      variance: "quiet",
      active_agents: 0,
      needs_count: 0,
      feature_counts: {},
    },
    features: {},
  });

  const needs: NeedsYouItem[] = [
    makeNeed({ id: "need-one", project: "alpha", source: ASK_ONE }),
    makeNeed({
      id: "need-two",
      project: "beta",
      source: ASK_TWO,
      question: "How long should pane history be kept?",
      node: "core/two",
      stalled_agents: 0,
      waiting_minutes: 90,
      unblocks: [],
    }),
  ];
  if (options.withRescue !== false) {
    needs.push(
      makeNeed({
        id: "rescue-one",
        kind: "rescue",
        project: "gamma",
        source: ulid(103),
        question: "Agent stopped mid-task",
        node: "core/one",
        stalled_agents: 0,
        waiting_minutes: 22,
        unblocks: [],
        reason: "Process exited while running fixtures",
      }),
    );
  }
  alpha.needs_you = [needs[0]!];
  beta.needs_you = [needs[1]!];
  gamma.needs_you = needs[2] ? [needs[2]] : [];
  delta.needs_you = [];

  return {
    v: 1,
    epoch: ulid(900),
    navigation_epoch: ulid(901),
    generation: 7,
    state: {
      projects: { alpha, beta, gamma, delta },
      diagnostics: [],
      event_set_digest: sha(0x99),
      evaluated_at: EVALUATED_AT,
    },
    needs,
    brief: {
      delivered: [],
      coming_up: [
        {
          id: "coming",
          text: "alpha: CLI ask likely done ~16:00",
          target: { kind: "project", id: "alpha" },
          source_ids: [ulid(201)],
        },
      ],
      needs_you: [
        {
          id: "needs",
          text: "alpha: 1 decision, 2 agents waiting",
          target: { kind: "need", id: "need-one" },
          source_ids: [ASK_ONE],
        },
      ],
      if_you_wait: [
        {
          id: "wait",
          text: "alpha: core/two stays blocked",
          target: { kind: "project", id: "alpha" },
          source_ids: [ASK_ONE],
        },
      ],
      by_default: [
        {
          id: "default",
          text: "alpha goes with Use harness adapters at 18:22",
          target: { kind: "need", id: "need-one" },
          source_ids: [ASK_ONE],
        },
      ],
      at_risk: [
        {
          id: "risk",
          text: "beta is past the agent's estimate",
          target: { kind: "project", id: "beta" },
          source_ids: [ASK_TWO],
        },
      ],
    },
    local: {
      host: "h-fixture",
      received_at: EVALUATED_AT,
      health: "healthy",
      diagnostics: [],
      capabilities: [],
      notifications: "granted",
      notification_actions: { availability: "available" },
    },
    preferences: basePreferences(),
  };
}

function riskOf(card: DecisionCard): Reversibility {
  return cardRisk(card);
}

/** A snapshot whose `alpha` project holds a delivery waiting for review. */
export function makeDeliverySnapshot(): Snapshot {
  const snapshot = makeSnapshot();
  const fixture = makeDeliveryProject();
  snapshot.state.projects.alpha = fixture.project;
  snapshot.needs = [fixture.need, ...snapshot.needs];
  return snapshot;
}

/**
 * Dry-run plans the first-run Setup sees, shaped like the host contract. Paths
 * are neutral fixture locations; the timeouts match the hook installer.
 */
export const PATH_LINK_PLAN: InstallPlan = {
  changes: [],
  requires_consent: true,
  plan_digest: sha(0xa1),
  already_installed: false,
  destination: "/opt/agendar-fixture/local-bin/agendar",
  command:
    "ln -s '/opt/agendar-fixture/bin/agendar' '/opt/agendar-fixture/local-bin/agendar'",
  cli_path: "/opt/agendar-fixture/bin/agendar",
};

export const HOOK_PLAN: InstallPlan = {
  harness: "claude_code",
  settings_path: "/opt/agendar-fixture/claude/settings.json",
  changes: [
    { event: "SessionStart", command: "'/opt/agendar-fixture/bin/agendar' hook session-start", timeout_s: 60 },
    { event: "PostToolUse", command: "'/opt/agendar-fixture/bin/agendar' hook post-tool-use", timeout_s: 10 },
    { event: "Stop", command: "'/opt/agendar-fixture/bin/agendar' hook stop", timeout_s: 60 },
  ],
  requires_consent: true,
  plan_digest: sha(0xa2),
  already_installed: false,
};


export const ICLOUD_LOCATION: SetupLocation = {
  id: "icloud-drive",
  kind: "icloud_drive",
  label: "iCloud Drive",
  path: "/opt/agendar-fixture/icloud/Agendar",
  folder_name: "Agendar",
};

export const LOCAL_LOCATION: SetupLocation = {
  id: "local",
  kind: "local",
  label: "This computer",
  path: "/opt/agendar-fixture/Documents/Agendar",
  folder_name: "Agendar",
};

export const VAULT_RESULT: ConfigureVaultData = {
  vault_id: ulid(300),
  host: "h01abcdefghjkmnpqrstvwxyz",
};

/**
 * Fake typed client used by every component test. It records calls, emits push
 * events and mutates its snapshot the way the real backend would after a
 * committed event, so views never need optimistic local state.
 */
export class FakeClient implements AgendarClient {
  readonly mode = "tauri" as const;
  snapshot: Snapshot;
  calls: { command: string; request: unknown }[] = [];
  order: string[] = [];
  subscribeOrder: string[] = [];
  unsubscribed: string[] = [];
  snapshotCalls = 0;
  subscribeCountAtFirstFetch: number | null = null;
  snapshotError: Error | null = null;
  answerError: Error | null = null;
  prepareError: Error | null = null;
  reviewError: Error | null = null;
  ruleError: Error | null = null;
  reportError: Error | null = null;
  approveError: Error | null = null;
  runError: Error | null = null;
  /** First-run setup knobs: Vault result and per-plan install behavior. */
  vaultResult: ConfigureVaultData = structuredClone(VAULT_RESULT);
  vaultError: Error | null = null;
  setupLocations: SetupLocation[] = [];
  setupLocationsError: Error | null = null;
  pathLinkPlan: InstallPlan = structuredClone(PATH_LINK_PLAN);
  pathLinkPlanError: Error | null = null;
  pathLinkInstallError: Error | null = null;
  hookPlan: InstallPlan = structuredClone(HOOK_PLAN);
  hookPlanError: Error | null = null;
  hookInstallError: Error | null = null;
  setPreferencesError: Error | null = null;
  /** Result reported by `run_check`; never a fabricated Seen on its own. */
  checkResult: CheckResult = "passed";
  /** Approved profiles in approval order. */
  readonly approvals: unknown[] = [];
  /** Blob bodies answered by `read_report`, keyed by digest. */
  readonly reports = new Map<string, ReadReportData>();
  guardOverride: "easy" | "choose" | "confirm" | null = null;
  /** Graph answered by `get_project`; tests publish their DTO here. */
  graph: Graph = makeGraph();
  /** request id → exact canonical payload; a different retry is a conflict. */
  readonly journal = new Map<string, string>();
  private readonly listeners = new Map<string, Set<(payload: unknown) => void>>();
  private deferred: {
    promise: Promise<void>;
    resolve: () => void;
  } | null = null;
  private tokens = new Map<string, PrepareAnswerRequest>();

  constructor(snapshot: Snapshot = makeSnapshot()) {
    this.snapshot = structuredClone(snapshot);
  }

  private record(command: string, request: unknown): void {
    this.calls.push({ command, request });
    this.order.push(command);
  }

  /** Hold the next get_snapshot until release() is called. */
  holdSnapshot(): void {
    let resolve!: () => void;
    const promise = new Promise<void>((done) => {
      resolve = done;
    });
    this.deferred = { promise, resolve };
  }

  releaseSnapshot(): void {
    this.deferred?.resolve();
    this.deferred = null;
  }

  setSnapshot(snapshot: Snapshot): void {
    this.snapshot = structuredClone(snapshot);
  }

  localConfig: LocalConfig = {
    vault: "/srv/agendar-vault",
    data_dir: "/srv/agendar-data",
    config_revision: sha(0x77),
    repos: { "/srv/code/alpha": "alpha" },
    codex_bin: null,
    cli_path: "/opt/agendar-fixture/bin/agendar",
    app_version: "0.1.0",
  };

  async getLocalConfig(): Promise<LocalConfig> {
    this.record("get_local_config", {});
    return structuredClone(this.localConfig);
  }

  /** `null`: the read fails. */
  storageUsage: StorageUsage | null = {
    evaluated_at: "2026-01-01T00:00:00.000Z",
    superseded_after_days: 30,
    total: emptyUsage(),
    projects: [],
  };

  async getStorageUsage(): Promise<StorageUsage> {
    this.record("get_storage_usage", {});
    if (!this.storageUsage) throw new ApiError({ code: "E_IO", message: "Cannot list the Vault.", hint: "", retryable: true });
    return structuredClone(this.storageUsage);
  }

  testNoticeResult: TestNoticeData = { sent: true };

  bindings: Binding[] = [
    { harness: "claude_code", kind: "hooks", state: "bound", found: true, path: "/home/u/.claude/settings.json", detail: "Hooks installed." },
    { harness: "codex", kind: "hooks", state: "stale", found: true, path: "/home/u/.codex/hooks.json", detail: "rebind" },
    { harness: "cursor", kind: "mcp", state: "not_bound", found: true, path: "/home/u/.cursor/mcp.json", detail: "No agendar MCP server." },
    { harness: "gemini", kind: "mcp", state: "not_bound", found: false, path: "/home/u/.gemini/settings.json", detail: "" },
    { harness: "dsh", kind: "manual", state: "manual", found: true, detail: "Add an MCP server row by hand." },
  ];

  async getBindings(): Promise<GetBindingsData> {
    this.record("get_bindings", {});
    return { bindings: this.bindings.map((binding) => ({ ...binding })), cli_path: "/Applications/Agendar.app/Contents/MacOS/agendar" };
  }

  async setBinding(request: SetBindingRequest): Promise<SetBindingData> {
    this.record("set_binding", request);
    const binding = this.bindings.find((item) => item.harness === request.harness)!;
    binding.state = request.action === "bind" ? "bound" : "not_bound";
    return { binding: { ...binding } };
  }

  async openReport(request: OpenReportRequest): Promise<OpenReportData> {
    this.record("open_report", request);
    return { opened: true };
  }

  async testNotice(): Promise<TestNoticeData> {
    this.record("test_notice", {});
    return structuredClone(this.testNoticeResult);
  }

  async getSetupLocations(): Promise<GetSetupLocationsData> {
    this.record("get_setup_locations", {});
    if (this.setupLocationsError) {
      const error = this.setupLocationsError;
      this.setupLocationsError = null;
      throw error;
    }
    return { locations: structuredClone(this.setupLocations) };
  }

  async getSnapshot(): Promise<Snapshot> {
    this.record("get_snapshot", {});
    this.snapshotCalls += 1;
    if (this.subscribeCountAtFirstFetch === null) {
      this.subscribeCountAtFirstFetch = this.subscribeOrder.length;
    }
    if (this.snapshotError) {
      const error = this.snapshotError;
      this.snapshotError = null;
      throw error;
    }
    const response = structuredClone(this.snapshot);
    if (this.deferred) await this.deferred.promise;
    return response;
  }

  async getProject(request: { project: string }) {
    this.record("get_project", request);
    const project = this.snapshot.state.projects[request.project];
    if (!project) throw new ApiError({ code: "E_NOT_FOUND", message: "Missing", hint: "", retryable: false });
    return { project: structuredClone(project), graph: structuredClone(this.graph) };
  }

  async search(request: { text: string }) {
    this.record("search", request);
    const text = request.text.toLowerCase();
    return {
      needs: this.snapshot.needs.filter((item) => item.question.toLowerCase().includes(text)),
      projects: Object.values(this.snapshot.state.projects)
        .map((project) => project.summary)
        .filter((summary) => summary.slug.includes(text)),
    };
  }

  async prepareAnswer(request: PrepareAnswerRequest): Promise<PrepareAnswerData> {
    this.record("prepare_answer", request);
    if (this.prepareError) throw this.prepareError;
    const decision = this.findDecision(request.ask_id);
    if (!decision) {
      throw new ApiError({
        code: "E_WITHDRAWN",
        message: "Decision was withdrawn or superseded.",
        hint: "Open the current card.",
        retryable: false,
      });
    }
    if (request.expected_decision_revision !== decision.revision) {
      throw new ApiError({
        code: "E_STALE_ANSWER",
        message: "Answer is no longer current.",
        hint: "Refresh the card before answering.",
        retryable: false,
      });
    }
    const option =
      request.option_id === "own" && request.own_answer
        ? ownOption(decision.card, request.own_answer)
        : decision.card.options.find((item) => item.id === request.option_id);
    if (!option) {
      throw new ApiError({
        code: "E_VALIDATION",
        message: "Unknown option.",
        hint: "Pick a shown option.",
        retryable: false,
      });
    }
    const token = `token-${this.tokens.size + 1}`;
    this.tokens.set(token, request);
    const risk = riskOf(decision.card);
    return {
      token,
      expires_at: new Date(Date.now() + 60_000).toISOString(),
      risk,
      label: option.label,
      receipt: {
        preview: true,
        ask: decision.id,
        node: decision.node,
        source: "human",
        chose: option,
        ruled_out: decision.card.options.filter((item) => item.id !== option.id),
        done_when: decision.card.done_when,
        ...(option.eta ? { expected_eta: option.eta } : {}),
      },
      guard: this.guardOverride ?? (risk === "easy" ? "easy" : risk === "costly" ? "choose" : "confirm"),
    };
  }

  async answerDecision(request: AnswerDecisionRequest): Promise<AnswerDecisionData> {
    this.record("answer_decision", request);
    // A reserved request id is retried identically; different input is a conflict.
    const payload = JSON.stringify(request);
    const previous = this.journal.get(request.request_id);
    if (previous !== undefined && previous !== payload) {
      throw new ApiError({
        code: "E_REQUEST_CONFLICT",
        message: "This request id was already used with different input.",
        hint: "Start the action again deliberately.",
        retryable: false,
      });
    }
    this.journal.set(request.request_id, payload);
    if (this.answerError) throw this.answerError;
    const prepared = this.tokens.get(request.token);
    if (!prepared) {
      throw new ApiError({
        code: "E_STALE_ANSWER",
        message: "Answer is no longer current.",
        hint: "Refresh the card.",
        retryable: false,
      });
    }
    const found = this.findDecision(prepared.ask_id);
    if (!found) {
      throw new ApiError({
        code: "E_WITHDRAWN",
        message: "Decision was withdrawn or superseded.",
        hint: "Open the current card.",
        retryable: false,
      });
    }
    const option =
      prepared.option_id === "own" && prepared.own_answer
        ? ownOption(found.card, prepared.own_answer)
        : found.card.options.find((item) => item.id === prepared.option_id);
    if (!option) throw new Error("missing option");
    const receipt: Receipt = {
      id: ulid(400 + this.calls.length),
      ask: found.id,
      node: found.node,
      source: "human",
      chose: option,
      ruled_out: found.card.options.filter((item) => item.id !== option.id),
      done_when: request.done_when ?? found.card.done_when,
      ...(option.eta ? { expected_eta: option.eta } : {}),
      created_at: EVALUATED_AT,
    };
    this.commitAnswer(found, receipt);
    this.tokens.delete(request.token);
    return {
      event_id: receipt.id,
      receipt,
      awaiting_ack: true,
      released: ["core/two"],
    };
  }

  async reviewDelivery(request: ReviewDeliveryRequest): Promise<ReviewDeliveryData> {
    this.record("review_delivery", request);
    if (this.reviewError) throw this.reviewError;
    const delivery = this.findDelivery(request.delivery_id);
    if (!delivery) {
      throw new ApiError({
        code: "E_WITHDRAWN",
        message: "Delivery is no longer current.",
        hint: "Refresh the brief.",
        retryable: false,
      });
    }
    if (request.expected_delivery_revision !== delivery.revision) {
      throw new ApiError({
        code: "E_STALE_ANSWER",
        message: "The delivery changed since it was shown.",
        hint: "Review the refreshed evidence.",
        retryable: false,
      });
    }
    delivery.review = request.verdict === "accept" ? "accepted" : "sent_back";
    delivery.review_event = ulid(700);
    delivery.revision = sha(0x93);
    const need = this.snapshot.needs.find((item) => item.source === delivery.id);
    if (need) {
      this.snapshot.needs = this.snapshot.needs.filter((item) => item.id !== need.id);
    }
    return { event_id: delivery.review_event, released: ["core/two"] };
  }

  async resolveRescue(request: { item_id: string; expected_rescue_revision: string; action: "handoff" | "pause" }) {
    this.record("resolve_rescue", request);
    const need = this.snapshot.needs.find((item) => item.id === request.item_id);
    if (!need) {
      throw new ApiError({
        code: "E_STALE_ANSWER",
        message: "Rescue item is no longer current.",
        hint: "Refresh.",
        retryable: false,
      });
    }
    const project = this.snapshot.state.projects[need.project];
    if (project) {
      const feature = need.node ? project.features[need.node] : undefined;
      if (feature) {
        if (request.action === "pause") feature.paused = true;
        else feature.liveness = "quiet";
      }
      project.needs_you = project.needs_you.filter((item) => item.id !== need.id);
      project.summary.needs_count = project.needs_you.length;
    }
    this.snapshot.needs = this.snapshot.needs.filter((item) => item.id !== need.id);
    return { event_id: ulid(500) };
  }

  async updateProject(request: unknown) {
    this.record("update_project", request);
    return { event_id: ulid(501) };
  }

  async updateFeature(request: unknown) {
    this.record("update_feature", request);
    return { event_id: ulid(502) };
  }

  /** Thrown by `dropSteps` when set; nothing is dropped then. */
  dropError: Error | null = null;

  async dropSteps(request: { nodes: string[] }) {
    this.record("drop_steps", request);
    if (this.dropError) throw this.dropError;
    return { event_ids: request.nodes.map((_, index) => ulid(600 + index)) };
  }

  async setPolicy(request: unknown) {
    this.record("set_policy", request);
    return { event_id: ulid(503) };
  }

  async setRule(request: { request_id: string; project: string; expected_revision: string; rule: unknown }) {
    this.record("set_rule", request);
    if (this.ruleError) throw this.ruleError;
    return { event_id: ulid(504) };
  }

  async readReport(request: ReadReportRequest): Promise<ReadReportData> {
    this.record("read_report", request);
    if (this.reportError) throw this.reportError;
    const known = this.reports.get(request.blob.sha256);
    if (known) return known;
    return { mime: "text/plain", sha256: request.blob.sha256 };
  }

  async approveCheck(request: ApproveCheckRequest): Promise<ApproveCheckData> {
    this.record("approve_check", request);
    if (this.approveError) throw this.approveError;
    this.approvals.push(request.profile);
    // The real command returns the profile digest (a 64-hex revision).
    return { approval_id: sha(0xa0 + this.approvals.length) };
  }

  async runCheck(request: RunCheckRequest): Promise<RunCheckData> {
    this.record("run_check", request);
    if (this.runError) throw this.runError;
    if (!this.approvals.length) {
      throw new ApiError({
        code: "E_UNAPPROVED",
        message: "That check has not been approved.",
        hint: "Approve the exact argv and cwd first.",
        retryable: false,
      });
    }
    return { observation_id: ulid(800 + this.calls.length), result: this.checkResult };
  }

  async setPreferences(request: SetPreferencesRequest): Promise<SetPreferencesData> {
    this.record("set_preferences", request);
    if (this.setPreferencesError) {
      const error = this.setPreferencesError;
      this.setPreferencesError = null;
      throw error;
    }
    const preferences: Preferences = {
      ...this.snapshot.preferences,
      ...(request.theme ? { theme: request.theme } : {}),
      ...(request.motion !== undefined ? { motion: request.motion } : {}),
      ...(request.notifications !== undefined ? { notifications: request.notifications } : {}),
      ...(request.update_check !== undefined ? { update_check: request.update_check } : {}),
      last_looked_at: request.mark_looked ? EVALUATED_AT : this.snapshot.preferences.last_looked_at,
      revision: sha(0x72),
    };
    this.snapshot.preferences = preferences;
    return { preferences: structuredClone(preferences) };
  }

  async dismissNeed(request: DismissNeedRequest): Promise<SetPreferencesData> {
    this.record("dismiss_need", request);
    const dismissed = [...new Set([...(this.snapshot.preferences.dismissed ?? []), request.item_id])];
    this.snapshot.preferences = { ...this.snapshot.preferences, dismissed, revision: sha(0x76) };
    return { preferences: structuredClone(this.snapshot.preferences) };
  }

  /** As the host does: the event is written and the reduced rows carry it. */
  async snoozeNeed(request: SnoozeNeedRequest): Promise<SnoozeNeedData> {
    this.record("snooze_need", request);
    const rows = [
      ...this.snapshot.needs,
      ...Object.values(this.snapshot.state.projects).flatMap((project) => project.needs_you),
    ];
    for (const row of rows) {
      if (row.id !== request.item_id) continue;
      if (Date.parse(request.until) > Date.parse(EVALUATED_AT)) row.snoozed_until = request.until;
      else delete row.snoozed_until;
    }
    return { event_id: ulid(900 + this.calls.length), snoozed_until: request.until };
  }

  async configureVault(request: ConfigureVaultRequest): Promise<ConfigureVaultData> {
    this.record("configure_vault", request);
    if (this.vaultError) {
      const error = this.vaultError;
      this.vaultError = null;
      throw error;
    }
    return structuredClone(this.vaultResult);
  }

  async installLocal(request: InstallLocalRequest): Promise<InstallLocalData> {
    this.record("install_local", request);
    if (request.dry_run) {
      const source = request.kind === "path_link" ? this.pathLinkPlan : this.hookPlan;
      const error = request.kind === "path_link" ? this.pathLinkPlanError : this.hookPlanError;
      if (error) {
        if (request.kind === "path_link") this.pathLinkPlanError = null;
        else this.hookPlanError = null;
        throw error;
      }
      return { plan: structuredClone(source), installed: false };
    }
    // Non-dry-run installs mirror the host consent and digest contract.
    if (!request.consent) {
      throw new ApiError({
        code: "E_CONSENT",
        message: "Explicit consent is required before installing.",
        hint: "",
        retryable: false,
      });
    }
    if (!request.expected_plan_digest) {
      throw new ApiError({
        code: "E_VALIDATION",
        message: "A non-dry-run install must echo the plan digest.",
        hint: "",
        retryable: false,
      });
    }
    const plan = request.kind === "path_link" ? this.pathLinkPlan : this.hookPlan;
    if (request.expected_plan_digest !== plan.plan_digest) {
      throw new ApiError({
        code: "E_STALE_ANSWER",
        message: "The plan changed after it was shown.",
        hint: "Review the new plan and consent again.",
        retryable: false,
      });
    }
    const error =
      request.kind === "path_link" ? this.pathLinkInstallError : this.hookInstallError;
    if (error) {
      if (request.kind === "path_link") this.pathLinkInstallError = null;
      else this.hookInstallError = null;
      throw error;
    }
    const installedPlan: InstallPlan = { ...plan, already_installed: true };
    if (request.kind === "path_link") this.pathLinkPlan = installedPlan;
    else this.hookPlan = installedPlan;
    return { plan: structuredClone(installedPlan), installed: true };
  }

  unlinkError: Error | null = null;

  async linkRepo(request: { repo: string; project: string }): Promise<LinkRepoData> {
    this.record("link_repo", request);
    return {
      repo: request.repo,
      project: request.project,
      expected_config_revision: sha(0x73),
    };
  }

  async unlinkRepo(request: UnlinkRepoRequest): Promise<UnlinkRepoData> {
    this.record("unlink_repo", request);
    if (this.unlinkError) throw this.unlinkError;
    const project = this.localConfig.repos[request.repo] ?? request.expected_project ?? null;
    const removed = request.repo in this.localConfig.repos;
    delete this.localConfig.repos[request.repo];
    const project_still_linked = project !== null && Object.values(this.localConfig.repos).includes(project);
    return { repo: request.repo, removed, project, project_still_linked, expected_config_revision: sha(0x74) };
  }

  async showMain(target?: { project: string; item_id: string } | { view: "about" }): Promise<void> {
    this.record("show_main", target ?? {});
  }

  async hidePopover(): Promise<void> {
    this.record("hide_popover", {});
  }

  chrome: WindowChrome = { overlay_titlebar: false, material: "solid" };

  async windowChrome(): Promise<WindowChrome> {
    this.record("window_chrome", {});
    return this.chrome;
  }

  async setGlassRegions(regions: GlassRegion[], tint: GlassTint | null): Promise<void> {
    this.record("set_glass_regions", tint ? { regions, tint } : { regions });
  }

  async checkUpdates(): Promise<CheckUpdatesData> {
    this.record("check_updates", {});
    if (!this.snapshot.update) throw new Error("no update status in this fixture");
    this.snapshot.update = { ...this.snapshot.update, checked_at: EVALUATED_AT };
    return { update: structuredClone(this.snapshot.update) };
  }

  async openRelease(): Promise<OpenReleaseData> {
    this.record("open_release", {});
    return { opened: true };
  }

  async openPreview(): Promise<never> {
    throw new ApiError({
      code: "E_NOT_IMPLEMENTED",
      message: "Capability open_preview is not implemented in this version.",
      hint: "Previews arrive in a later version.",
      retryable: false,
    });
  }

  async onChanged(handler: (event: ChangedEvent) => void): Promise<UnlistenFn> {
    this.subscribeOrder.push("changed");
    return this.addListener("agendar://changed", handler);
  }

  async onHealth(handler: (event: EventMap["agendar://health"]) => void): Promise<UnlistenFn> {
    this.subscribeOrder.push("health");
    return this.addListener("agendar://health", handler);
  }

  async onUnblock(handler: (event: EventMap["agendar://unblock"]) => void): Promise<UnlistenFn> {
    this.subscribeOrder.push("unblock");
    return this.addListener("agendar://unblock", handler);
  }

  async onNotificationFocus(handler: (event: EventMap["agendar://notification-focus"]) => void): Promise<UnlistenFn> {
    this.subscribeOrder.push("focus");
    return this.addListener("agendar://notification-focus", handler);
  }

  async onUpdate(handler: (event: EventMap["agendar://update"]) => void): Promise<UnlistenFn> {
    this.subscribeOrder.push("update");
    return this.addListener("agendar://update", handler);
  }

  async onUpload(handler: (event: EventMap["agendar://upload"]) => void): Promise<UnlistenFn> {
    this.subscribeOrder.push("upload");
    return this.addListener("agendar://upload", handler);
  }

  async onNavigate(handler: (event: EventMap["agendar://navigate"]) => void): Promise<UnlistenFn> {
    this.subscribeOrder.push("navigate");
    return this.addListener("agendar://navigate", handler);
  }

  emit<K extends keyof EventMap>(event: K, payload: EventMap[K]): void {
    for (const handler of this.listeners.get(event) ?? []) handler(payload);
  }

  private addListener<P>(event: string, handler: (payload: P) => void): UnlistenFn {
    const set = this.listeners.get(event) ?? new Set<(payload: unknown) => void>();
    set.add(handler as unknown as (payload: unknown) => void);
    this.listeners.set(event, set);
    return () => {
      set.delete(handler as unknown as (payload: unknown) => void);
      this.unsubscribed.push(event);
    };
  }

  private findDecision(askId: string): DecisionState | undefined {
    for (const project of Object.values(this.snapshot.state.projects)) {
      const decision = project.decisions[askId];
      if (decision) return decision;
    }
    return undefined;
  }

  private findDelivery(deliveryId: string): DeliveryState | undefined {
    for (const project of Object.values(this.snapshot.state.projects)) {
      const delivery = project.deliveries[deliveryId];
      if (delivery) return delivery;
    }
    return undefined;
  }

  private commitAnswer(decision: DecisionState, receipt: Receipt): void {
    const project = Object.values(this.snapshot.state.projects).find((item) => item.decisions[decision.id]);
    if (!project) return;
    const current = project.decisions[decision.id]!;
    current.lifecycle = "answered";
    current.effective_receipt = receipt.id;
    current.answer_history = [...current.answer_history, receipt.id];
    project.receipts[receipt.id] = receipt;
    const need = this.snapshot.needs.find((item) => item.source === decision.id);
    if (need) {
      this.snapshot.needs = this.snapshot.needs.filter((item) => item.id !== need.id);
      project.needs_you = project.needs_you.filter((item) => item.id !== need.id);
      project.summary.needs_count = project.needs_you.length;
    }
    this.snapshot.generation += 1;
  }
}

/** Transport wrapper for client-level tests. */
export function transportFromClient(client: FakeClient): Transport {
  return {
    invoke: (command, request) => {
      switch (command) {
        case "get_snapshot":
          return client.getSnapshot();
        default:
          return Promise.reject(new Error(`Unhandled fake invoke ${command} ${String(request)}`));
      }
    },
    listen: (event, handler) => {
      switch (event) {
        case "agendar://changed":
          return client.onChanged(handler as (event: ChangedEvent) => void);
        case "agendar://health":
          return client.onHealth(handler as (event: EventMap["agendar://health"]) => void);
        case "agendar://unblock":
          return client.onUnblock(handler as (event: EventMap["agendar://unblock"]) => void);
        case "agendar://notification-focus":
          return client.onNotificationFocus(
            handler as (event: EventMap["agendar://notification-focus"]) => void,
          );
        case "agendar://update":
          return client.onUpdate(handler as (event: EventMap["agendar://update"]) => void);
        case "agendar://upload":
          return client.onUpload(handler as (event: EventMap["agendar://upload"]) => void);
        case "agendar://navigate":
          return client.onNavigate(handler as (event: EventMap["agendar://navigate"]) => void);
        default:
          return Promise.resolve(() => undefined);
      }
    },
  };
}

/** Usage numbers with nothing in them; tests fill in what they show. */
export function emptyUsage(): UsageNumbers {
  const none = { count: 0, bytes: 0, local_bytes: 0 };
  return {
    total_bytes: 0,
    total_local_bytes: 0,
    event_bytes: 0,
    event_local_bytes: 0,
    attachments: { ...none },
    categories: {},
    reclaimable: { ...none },
    superseded: { ...none },
    unreferenced: { ...none },
  };
}
