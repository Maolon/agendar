/**
 * Showcase data for the README screenshots (`VITE_AGENDAR_FIXTURES=showcase`).
 * Entirely invented, calm and small: three projects on track, one waiting
 * question with three short options. Development only, like the test fixtures;
 * `vite build` strips it.
 */

import type {
  ActivityItem,
  DecisionCard,
  DecisionState,
  NeedsYouItem,
  ProjectState,
  Snapshot,
} from "../api/types";
import { feature, sha, simpleProject, ulid } from "./fixtures";

const NOW = "2026-02-10T15:00:00.000Z";
const HOST = "h-01h0000000000000000000000";
const READER = ulid(801);
const RATES = ulid(802);
const NOTES = ulid(803);

const CONFLICT_CARD: DecisionCard = {
  why_now: "Sync is the last piece before highlights can ship.",
  question: "How should two devices settle an edit conflict?",
  reversibility: "easy",
  reach: "local",
  external: false,
  domain: "arch",
  options: [
    {
      id: "merge",
      label: "Merge by paragraph",
      consequence: "Keeps both edits unless they touch the same paragraph",
      reversibility: "easy",
      external: false,
      eta: { min_s: 7200, max_s: 10800 },
      icon: "git-branch",
    },
    {
      id: "latest",
      label: "Latest edit wins",
      consequence: "Simplest; an older edit can be lost",
      reversibility: "easy",
      external: false,
      eta: { min_s: 1800, max_s: 3600 },
      icon: "clock",
    },
    {
      id: "ask",
      label: "Ask the reader",
      consequence: "Shows both versions side by side",
      reversibility: "easy",
      external: false,
      eta: { min_s: 3600, max_s: 7200 },
      icon: "user",
    },
  ],
  recommended: {
    option: "merge",
    confidence: "high",
    because: "Readers rarely edit the same paragraph twice.",
  },
  already_checked: [],
  done_when: [{ id: "sync", text: "Two devices edit offline and both edits survive" }],
  on_timeout: { kind: "hold" },
};

const CONFLICT: DecisionState = {
  id: ulid(811),
  node: "sync",
  card: CONFLICT_CARD,
  mode: "ask",
  lifecycle: "open",
  answer_history: [],
  ack_history: [],
  built_on: [],
  pending_pivot: false,
  revision: sha(0xa1),
};

const CONFLICT_NEED: NeedsYouItem = {
  id: "decide:sync-conflicts",
  kind: "decide",
  project: "lantern",
  node: "sync",
  source: CONFLICT.id,
  question: CONFLICT_CARD.question,
  since: "2026-02-10T14:48:00.000Z",
  stalled_agents: 1,
  waiting_minutes: 12,
  weight_x2: 2,
  score_x2: 24,
  reason: "1 agent waiting · 12m",
  unblocks: [],
  revision: sha(0xa2),
};

function eta(anchor: string, earliest: string, latest: string, minS: number, maxS: number) {
  return {
    agent_range: { min_s: minS, max_s: maxS },
    anchor,
    paused_s: 0,
    agent_finish: { earliest, latest },
    late_by_s: 0,
    paused: false,
  };
}

function activity(items: [string, string, string, string][]): ActivityItem[] {
  return items.map(([ts, actor, icon, text]) => ({
    ts,
    actor,
    actor_kind: actor === "You" ? "human" : "agent",
    icon,
    text,
    count: 1,
  }));
}

function lantern(): ProjectState {
  const project = simpleProject({
    slug: "lantern",
    title: "Lantern",
    brief: "A calm reading app for long articles",
    goal: "Read offline on every device, highlights included",
    glyph: "library",
    variance: "active",
    active_agents: 1,
    needs: [CONFLICT_NEED],
    decisions: { [CONFLICT.id]: CONFLICT },
    sessions: {
      [READER]: {
        id: READER,
        host: HOST,
        harness: "claude_code",
        last_event: ulid(821),
        last_event_ts: "2026-02-10T14:57:00.000Z",
        current_node: "reader/typography",
        liveness: "live",
        stalled_on: [],
      },
    },
    features: {
      "sync/model": feature({ node: "sync/model", title: "Sync data model", slice: "Sync", progress: "verified" }),
      "reader/typography": feature({
        node: "reader/typography",
        title: "Reading typography",
        slice: "Reader",
        progress: "doing",
        liveness: "live",
        attempt: ulid(822),
        session: READER,
        last_pulse: ulid(821),
        eta: eta("2026-02-10T14:20:00.000Z", "2026-02-10T15:20:00.000Z", "2026-02-10T15:50:00.000Z", 3600, 5400),
      }),
      "reader/highlights": feature({ node: "reader/highlights", title: "Highlights", slice: "Reader" }),
    },
  });
  project.activity = activity([
    ["2026-02-10T14:57:00.000Z", "Claude Code", "radar", "Line length and spacing tuned for long reads"],
    ["2026-02-10T14:48:00.000Z", "Claude Code", "git-branch", "Asked how two devices should settle an edit conflict"],
    ["2026-02-10T13:40:00.000Z", "Claude Code", "check", "Finished Sync data model"],
  ]);
  project.stats = {
    agents_running: 1,
    agents_idle: 0,
    agents_stalled: 0,
    finished_24h: 3,
    agent_ms_24h: 4 * 3600 * 1000 + 10 * 60 * 1000,
    median_step_ms: 52 * 60 * 1000,
    timed_steps: 3,
    instant_excluded: 0,
    tokens: { input: 420_000, cached: 1_800_000, output: 61_000 },
  };
  return project;
}

function harbor(): ProjectState {
  const project = simpleProject({
    slug: "harbor",
    title: "Harbor",
    brief: "Shipping quotes for small online shops",
    goal: "Quote three carriers in under a second",
    glyph: "server",
    variance: "active",
    active_agents: 1,
    sessions: {
      [RATES]: {
        id: RATES,
        host: HOST,
        harness: "codex",
        last_event: ulid(831),
        last_event_ts: "2026-02-10T14:55:00.000Z",
        current_node: "rates/cache",
        liveness: "live",
        stalled_on: [],
      },
    },
    features: {
      "rates/carriers": feature({ node: "rates/carriers", title: "Carrier rates", slice: "Rates", progress: "verified", unblocks: ["rates/cache"] }),
      "rates/cache": feature({
        node: "rates/cache",
        title: "Rate cache",
        slice: "Rates",
        progress: "doing",
        liveness: "live",
        attempt: ulid(832),
        session: RATES,
        last_pulse: ulid(831),
        depends_on: ["rates/carriers"],
        unblocks: ["rates/checkout"],
        eta: eta("2026-02-10T14:10:00.000Z", "2026-02-10T15:40:00.000Z", "2026-02-10T16:10:00.000Z", 5400, 7200),
      }),
      "rates/checkout": feature({ node: "rates/checkout", title: "Checkout widget", slice: "Checkout", depends_on: ["rates/cache"] }),
    },
  });
  project.activity = activity([
    ["2026-02-10T14:55:00.000Z", "Codex", "radar", "Cache hit rate 94% on the sample orders"],
    ["2026-02-10T12:30:00.000Z", "Codex", "check", "Finished Carrier rates"],
  ]);
  return project;
}

function fieldNotes(): ProjectState {
  return simpleProject({
    slug: "field-notes",
    title: "Field notes",
    brief: "A notebook for field research teams",
    goal: "Photos and notes reach the whole team",
    glyph: "file-text",
    variance: "quiet",
    sessions: {
      [NOTES]: {
        id: NOTES,
        host: HOST,
        harness: "claude_code",
        last_event: ulid(841),
        last_event_ts: "2026-02-10T11:05:00.000Z",
        liveness: "quiet",
        stalled_on: [],
      },
    },
    features: {
      "notes/capture": feature({ node: "notes/capture", title: "Photo capture", slice: "Capture", progress: "verified" }),
      "notes/share": feature({ node: "notes/share", title: "Team sharing", slice: "Share", progress: "verified" }),
    },
  });
}

export function buildShowcaseSnapshot(): Snapshot {
  return structuredClone({
    v: 1,
    epoch: ulid(950),
    navigation_epoch: ulid(951),
    generation: 3,
    state: {
      projects: { lantern: lantern(), harbor: harbor(), "field-notes": fieldNotes() },
      diagnostics: [],
      event_set_digest: sha(0xb9),
      evaluated_at: NOW,
    },
    needs: [CONFLICT_NEED],
    brief: {
      delivered: [
        {
          id: "delivered:carriers",
          text: "Harbor: carrier rates are verified",
          target: { kind: "project", id: "harbor" },
          source_ids: [ulid(833)],
        },
      ],
      coming_up: [
        {
          id: "coming:typography",
          text: "Lantern: Reading typography likely done ~15:50",
          target: { kind: "project", id: "lantern" },
          source_ids: [ulid(821)],
        },
        {
          id: "coming:cache",
          text: "Harbor: Rate cache likely done ~16:10",
          target: { kind: "project", id: "harbor" },
          source_ids: [ulid(831)],
        },
      ],
      needs_you: [
        {
          id: "needs:conflicts",
          text: "Lantern: 1 question, 1 agent waiting",
          target: { kind: "need", id: CONFLICT_NEED.id },
          source_ids: [CONFLICT.id],
        },
      ],
      if_you_wait: [],
      by_default: [],
      at_risk: [],
    },
    local: {
      host: HOST,
      received_at: NOW,
      health: "healthy",
      diagnostics: [],
      capabilities: [],
      notifications: "granted",
      notification_actions: { availability: "available" },
    },
    preferences: {
      theme: "system",
      motion: true,
      last_looked_at: "2026-02-10T14:00:00.000Z",
      last_looked_ids: [],
      revision: sha(0xb1),
    },
    // A local build: neutral info only, so the screenshots carry no update row.
    update: {
      current: "0.1.0",
      channel: "dev",
      latest: "0.1.0",
      available: false,
      url: "https://github.com/Maolon/agendar/releases/tag/v0.1.0",
      published_at: "2026-02-01T10:00:00Z",
      checked_at: "2026-02-10T14:00:00.000Z",
      enabled: true,
    },
  } satisfies Snapshot);
}
