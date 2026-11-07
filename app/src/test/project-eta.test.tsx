import { formatEtaFinish, formatFinishWindow, formatRemaining } from "../api/format";
import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ClientContext } from "../api/store";
import type { DecisionCard as DecisionCardDto, FeatureState, Graph, ProjectState } from "../api/types";
import { DecisionCard } from "../components/DecisionCard";
import { Project } from "../views/Project";
import {
  FakeClient,
  easyCard,
  makeGraph,
  makeNeed,
  makeProject,
  projectProps,
  sha,
  ulid,
} from "./fixtures";

const ANCHOR = "2026-02-10T15:00:00.000Z";
const SINCE = "2026-02-10T14:22:00.000Z";
const DEFAULT_AT = "2026-02-10T18:22:00.000Z";

function renderCard(card: DecisionCardDto = easyCard()) {
  render(
    <DecisionCard
      project="alpha"
      glyph="radar"
      slice="Slice 1 · Ask"
      card={card}
      mode="ask"
      anchor={ANCHOR}
      since={SINCE}
      defaultAt={DEFAULT_AT}
      stalledAgents={1}
      selected="one"
      explicit
      onSelect={vi.fn()}
      doneWhen={card.done_when}
      onDoneWhen={vi.fn()}
      receiptTag="Preview · not saved"
      receiptSaved={false}
      unblockLabels={[]}
      primary={{
        label: "Use harness adapters",
        onClick: vi.fn(),
        disabled: false,
        kbd: "enter",
      }}
      confirm={null}
      pending={false}
      canMutate
    />,
  );
}

describe("decision ETA texts", () => {
  it("shows each option's expected finish as Likely done", () => {
    renderCard();
    const expected = formatEtaFinish({ min_s: 3600, max_s: 7200 }, ANCHOR);
    expect(screen.getByText(`Likely done ${expected}`)).toBeInTheDocument();
  });

  it("states the answer-now versus wait-for-default trade-off", () => {
    renderCard();
    const finish = formatEtaFinish({ min_s: 3600, max_s: 7200 }, ANCHOR);
    const wait = formatRemaining(DEFAULT_AT, ANCHOR);
    expect(
      screen.getByText(
        `Answer now: likely done ${finish}. Wait for the default: about ${wait} from now.`,
      ),
    ).toBeInTheDocument();
  });

  it("records the expected finish in the receipt", () => {
    renderCard();
    expect(screen.getByText("Expected")).toBeInTheDocument();
    const finish = formatEtaFinish({ min_s: 3600, max_s: 7200 }, ANCHOR);
    expect(screen.getByText(`${finish} · agent says`)).toBeInTheDocument();
  });

  it("keeps the option's declared ETA on the card", () => {
    const card = easyCard();
    card.options[0] = {
      ...card.options[0]!,
      eta: { min_s: 1800, max_s: 3600 },
    };
    renderCard(card);
    const finish = formatEtaFinish({ min_s: 1800, max_s: 3600 }, ANCHOR);
    expect(screen.getByText(`Likely done ${finish}`)).toBeInTheDocument();
    expect(screen.getByText(/Recommended:/)).toBeInTheDocument();
  });
});

function etaFeature(overrides: Partial<FeatureState> = {}): FeatureState {
  return {
    ...makeProject("alpha").features["core/one"]!,
    progress: "doing",
    gate: "open",
    liveness: "live",
    ...overrides,
  };
}

describe("project plan and in-flight rows", () => {
  function makePlanProject(): ProjectState {
    const base = makeProject("alpha");
    const ask = ulid(101);
    return {
      ...base,
      summary: {
        ...base.summary,
        slug: "alpha",
        feature_counts: { verified: 1, planned: 2, doing: 1 },
        calibration: [{ harness: "claude_code", samples: 6, ratio_milli: 1500 }],
      },
      features: {
        "core/ask": {
          ...base.features["core/one"]!,
          node: "core/ask",
          title: "CLI ask",
          progress: "verified",
          gate: "open",
          liveness: "quiet",
          unblocks: ["core/ipc"],
        },
        "core/ipc": {
          ...base.features["core/one"]!,
          node: "core/ipc",
          title: "Wake method?",
          progress: "planned",
          gate: "awaiting_decision",
          liveness: "quiet",
          depends_on: ["core/ask"],
          waits_on: [`decision:${ask}`],
          unblocks: ["core/wake"],
        },
        "core/wake": {
          ...base.features["core/one"]!,
          node: "core/wake",
          title: "Wake idle agents",
          progress: "planned",
          gate: "awaiting_upstream",
          liveness: "quiet",
          depends_on: ["core/ipc"],
        },
        "core/menubar": etaFeature({
          node: "core/menubar",
          title: "Menu bar triage",
          session: ulid(201),
          eta: {
            agent_range: { min_s: 3600, max_s: 7200 },
            anchor: "2026-02-10T14:00:00.000Z",
            paused_s: 0,
            agent_finish: {
              earliest: "2026-02-10T15:30:00.000Z",
              latest: "2026-02-10T16:00:00.000Z",
            },
            late_by_s: 0,
            paused: false,
          },
        }),
      },
      decisions: {
        [ask]: {
          id: ask,
          node: "core/ipc",
          card: easyCard({ question: "How should answers reach idle agents?" }),
          mode: "ask",
          lifecycle: "open",
          answer_history: [],
          ack_history: [],
          default_at: DEFAULT_AT,
          built_on: [],
          pending_pivot: false,
          revision: sha(0x31),
        },
      },
      sessions: {
        [ulid(201)]: {
          id: ulid(201),
          host: "h-fixture",
          harness: "claude_code",
          last_event: ulid(202),
          last_event_ts: "2026-02-10T14:59:00.000Z",
          current_node: "core/menubar",
          liveness: "live",
          stalled_on: [],
        },
      },
      needs_you: [
        makeNeed({ id: "need-ipc", project: "alpha", source: ask, node: "core/ipc" }),
      ],
    };
  }

  /** The same plan as the DTO would publish it: features plus the pending call. */
  function makePlanGraph(project: ProjectState): Graph {
    const ask = Object.values(project.decisions)[0]!.id;
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
          id: `decision:${ask}`,
          kind: "decision",
          label: "Wake method?",
          need_id: "need-ipc",
          state: "pending",
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
      ],
      edges: [
        { from: "feature:core/ask", to: "feature:core/ipc", source_event: sha(0x61) },
        { from: `decision:${ask}`, to: "feature:core/ipc", source_event: sha(0x62) },
        { from: "feature:core/ipc", to: "feature:core/wake", source_event: sha(0x63) },
        { from: "feature:core/ask", to: "feature:core/menubar", source_event: sha(0x64) },
      ],
    });
  }

  it("lays the plan out in columns, shows waits as a Now card, and calibration", async () => {
    const client = new FakeClient();
    const project = makePlanProject();
    client.graph = makePlanGraph(project);
    render(
      <ClientContext.Provider value={client}>
        <Project {...projectProps(project, client)} anchor={ANCHOR} />
      </ClientContext.Provider>,
    );

    // The graph is fetched for the shown project; the plan list is replaced.
    await waitFor(() =>
      expect(document.querySelectorAll("[data-graph-node]")).toHaveLength(5),
    );
    const nodes = Array.from(document.querySelectorAll<HTMLElement>("[data-graph-node]")).map(
      (node) => node.dataset.graphNode,
    );
    // Input order is preserved; the decision sits in the root column.
    expect(nodes[0]).toBe("feature:core/ask");
    const depth = (id: string) =>
      document.querySelector<HTMLElement>(`[data-graph-node="${id}"]`)?.dataset.depth;
    expect(depth("feature:core/ask")).toBe("0");
    expect(depth(`decision:${Object.values(project.decisions)[0]!.id}`)).toBe("0");
    expect(depth("feature:core/ipc")).toBe("1");
    expect(depth("feature:core/menubar")).toBe("1");
    expect(depth("feature:core/wake")).toBe("2");

    // The pending call is an amber "Waits for you" card in Now, and the gated
    // step says so in the plan; nothing shows a raw host id.
    const waiting = document.querySelector<HTMLElement>(".ncard.you");
    expect(waiting?.textContent).toContain("How should answers reach idle agents?");
    expect(document.body.textContent).toContain("waits on your call");
    expect(screen.getAllByText("Wake idle agents").length).toBeGreaterThan(0);
    // The moving step names its harness, not its host.
    expect(document.querySelector(".ncard .chip")?.textContent).toBe("Claude Code");
    expect(document.body.textContent).not.toContain("h-fixture");
    expect(document.body.textContent).toContain(
      `ETA ${formatFinishWindow({
        earliest: "2026-02-10T15:30:00.000Z",
        latest: "2026-02-10T16:00:00.000Z",
      })}`,
    );
    expect(
      screen.getByText(
        "Agents here usually run 1.5× longer than they say, so these times are adjusted (last 6 slices, claude code).",
      ),
    ).toBeInTheDocument();
  });

  it("keeps the agent's original claim and the applied ratio next to the calibrated ETA on the Now card", () => {
    const project = makePlanProject();
    // Distinct agent claim and calibrated finish, plus a project-wide ratio
    // that differs from the feature's own so the row cannot borrow it.
    project.summary.calibration = [{ harness: "claude_code", samples: 6, ratio_milli: 2000 }];
    const feature = project.features["core/menubar"]!;
    project.features["core/menubar"] = {
      ...feature,
      eta: {
        ...feature.eta!,
        likely_finish: {
          earliest: "2026-02-10T15:45:00.000Z",
          latest: "2026-02-10T16:30:00.000Z",
        },
        ratio_milli: 1500,
      },
    };

    const client = new FakeClient();
    render(
      <ClientContext.Provider value={client}>
        <Project {...projectProps(project, client)} anchor={ANCHOR} />
      </ClientContext.Provider>,
    );

    const calibrated = formatFinishWindow({
      earliest: "2026-02-10T15:45:00.000Z",
      latest: "2026-02-10T16:30:00.000Z",
    });
    const original = formatFinishWindow({
      earliest: "2026-02-10T15:30:00.000Z",
      latest: "2026-02-10T16:00:00.000Z",
    });
    expect(calibrated).not.toEqual(original);
    const card = document.querySelector<HTMLElement>('.ncard[data-feat="core/menubar"]')!;
    // The calibrated window is the labelled "likely" window on the time line…
    expect(card.querySelector(".ntl-win-label")?.textContent).toMatch(/^likely \d\d:\d\d–\d\d:\d\d$/);
    // …with the agent's own claim, the feature's ratio and its evidence beside it.
    expect(card.textContent).toMatch(/Agent said \d\d:\d\d–\d\d:\d\d · adjusted 1\.5× from 6 finished steps/);
    expect(card.textContent).not.toContain("2×");
  });

  it("says calibration is unavailable instead of inventing a ratio", () => {
    const project = makePlanProject();
    project.summary.calibration = [];
    const client = new FakeClient();
    render(
      <ClientContext.Provider value={client}>
        <Project {...projectProps(project, client)} anchor={ANCHOR} />
      </ClientContext.Provider>,
    );
    expect(
      screen.getByText(
        "Not enough finished slices here to adjust yet; times are what the agents say.",
      ),
    ).toBeInTheDocument();
  });
});
