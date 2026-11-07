import type { Graph, ProjectState } from "../api/types";
import { easyCard, makeDecision, makeGraph, makeNeed, makeProject, ulid } from "./fixtures";

/** One project/graph pair shared by the graph view and motion tests. */
export const ASK = ulid(150);
const TAINT = ulid(151);

/** The prototype's agendar plan plus one tainted and one assumed feature. */
export function planProject(): ProjectState {
  const base = makeProject("alpha");
  const feature = base.features["core/one"]!;
  const decision = makeDecision(ASK, easyCard({ question: "How should answers reach idle agents?" }), {
    node: "core/ipc",
    lifecycle: "open",
  });
  return {
    ...base,
    features: {
      "core/ask": {
        ...feature,
        node: "core/ask",
        title: "CLI ask",
        progress: "verified",
        gate: "open",
        liveness: "quiet",
      },
      "core/ipc": {
        ...feature,
        node: "core/ipc",
        title: "Wake method?",
        progress: "planned",
        gate: "awaiting_decision",
        liveness: "quiet",
        depends_on: ["core/ask"],
        waits_on: [`decision:${ASK}`],
      },
      "core/wake": {
        ...feature,
        node: "core/wake",
        title: "Wake idle agents",
        progress: "planned",
        gate: "awaiting_upstream",
        liveness: "quiet",
        depends_on: ["core/ipc"],
      },
      "core/menubar": {
        ...feature,
        node: "core/menubar",
        title: "Menu bar triage",
        progress: "doing",
        gate: "open",
        basis: "firm",
        liveness: "live",
        paused: false,
      },
      "core/hotkeys": {
        ...feature,
        node: "core/hotkeys",
        title: "Hotkeys",
        progress: "planned",
        gate: "open",
        basis: "assumed",
        assumptions: [ulid(152)],
      },
      "core/async": {
        ...feature,
        node: "core/async",
        title: "Async wait",
        progress: "doing",
        gate: "open",
        basis: "tainted",
        taints: [TAINT],
        liveness: "live",
        eta: {
          agent_range: { min_s: 600, max_s: 900 },
          anchor: "2026-02-10T14:00:00.000Z",
          paused_s: 0,
          agent_finish: {
            earliest: "2026-02-10T14:15:00.000Z",
            latest: "2026-02-10T14:25:00.000Z",
          },
          late_by_s: 900,
          paused: false,
        },
      },
    },
    decisions: { [ASK]: decision },
    needs_you: [makeNeed({ id: "need-ipc", project: "alpha", source: ASK, node: "core/ipc" })],
    sessions: {},
  };
}

export function planGraph(): Graph {
  return makeGraph({
    nodes: [
      {
        id: "feature:core/ask",
        kind: "feature",
        label: "CLI ask",
        feature: "core/ask",
        state: "verified",
        assumed: false,
      },
      {
        id: `decision:${ASK}`,
        kind: "decision",
        label: "How should answers reach idle agents?",
        need_id: "need-ipc",
        state: "open",
        assumed: false,
      },
      {
        id: "feature:core/ipc",
        kind: "feature",
        label: "Wake method?",
        feature: "core/ipc",
        state: "planned",
        assumed: false,
      },
      {
        id: "feature:core/wake",
        kind: "feature",
        label: "Wake idle agents",
        feature: "core/wake",
        state: "planned",
        assumed: false,
      },
      {
        id: "feature:core/menubar",
        kind: "feature",
        label: "Menu bar triage",
        feature: "core/menubar",
        state: "doing",
        assumed: false,
      },
      {
        id: "feature:core/hotkeys",
        kind: "feature",
        label: "Hotkeys",
        feature: "core/hotkeys",
        state: "planned",
        assumed: true,
      },
      {
        id: "feature:core/async",
        kind: "feature",
        label: "Async wait",
        feature: "core/async",
        state: "doing",
        assumed: true,
      },
    ],
    edges: [
      { from: "feature:core/ask", to: "feature:core/ipc", source_event: ulid(160) },
      { from: `decision:${ASK}`, to: "feature:core/ipc", source_event: ASK },
      { from: "feature:core/ipc", to: "feature:core/wake", source_event: ulid(161) },
      { from: "feature:core/ask", to: "feature:core/menubar", source_event: ulid(162) },
    ],
  });
}

