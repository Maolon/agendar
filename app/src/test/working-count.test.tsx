import { render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ClientContext } from "../api/store";
import type { ClaimState, FeatureState, ProjectState, Snapshot } from "../api/types";
import { Sidebar } from "../components/Sidebar";
import { TrayPopover } from "../components/TrayPopover";
import { I18nProvider } from "../i18n";
import { nowBoard } from "../state/now";
import { agentCounts, agentsLine } from "../state/project";
import { FakeClient, makeProject, makeSnapshot, ulid } from "./fixtures";

const AT = "2026-02-10T14:00:00.000Z";
const HELD: ClaimState = { state: "held", since: AT, last_word: AT };
const SILENT: ClaimState = { ...HELD, state: "stalled" };
const ONE = ulid(801);
const TWO = ulid(802);

function step(node: string, overrides: Partial<FeatureState>): FeatureState {
  const base = makeProject("alpha").features["core/one"]!;
  return { ...base, node, title: node, depends_on: [], unblocks: [], gate: "open", progress: "doing", ...overrides };
}

/** A sent-back step whose live agent let its claim go: nobody is on it. */
const sentBack = (session: string) =>
  step("back", { phase: "sent_back", liveness: "live", session, claim: SILENT });
/** In progress, live, no word since its ETA passed. */
const silent = (session: string) => step("late", { liveness: "live", session, claim: SILENT });
const atWork = (session: string, node = "work") => step(node, { liveness: "live", session, claim: HELD });

function project(features: FeatureState[], active: number): ProjectState {
  const value = makeProject("alpha", { features: Object.fromEntries(features.map((item) => [item.node, item])) });
  value.summary.active_agents = active;
  value.summary.needs_count = 0;
  value.needs_you = [];
  return value;
}

function snapshotOf(value: ProjectState): Snapshot {
  const snapshot = makeSnapshot({ withRescue: false });
  snapshot.state.projects = { alpha: value };
  snapshot.needs = [];
  snapshot.brief.delivered = [];
  return snapshot;
}

/** What the tray header and the sidebar footer say about the agents. */
function surfaces(value: ProjectState) {
  const snapshot = snapshotOf(value);
  const tray = render(
    <ClientContext.Provider value={new FakeClient()}>
      <TrayPopover snapshot={snapshot} canMutate onOpen={vi.fn()} onAnswered={vi.fn()} onRefresh={vi.fn()} />
    </ClientContext.Provider>,
  );
  const side = render(
    <Sidebar snapshot={snapshot} selection={null} quietOpen={false} onSelect={vi.fn()} onToggleQuiet={vi.fn()} />,
  );
  const result = {
    tray: tray.container.querySelector(".pop-status")!.textContent!.split(" · ").slice(0, -1).join(" · "),
    trayLive: tray.container.querySelector(".pop-mark")!.classList.contains("live"),
    rows: [...tray.container.querySelectorAll(".tray-proj-line")].map((row) => row.textContent),
    sidebar: side.container.querySelector(".foot-agents .foot-text")!.textContent,
    board: nowBoard(snapshot),
  };
  tray.unmount();
  side.unmount();
  return result;
}

describe("the working count agrees with Now's lists", () => {
  it("counts no working agent when the only live agent's step was sent back and left", () => {
    const value = project([sentBack(ONE)], 1);
    const seen = surfaces(value);
    expect(seen.board.moving).toHaveLength(0);
    expect(seen.board.stalled[0]!.steps.map((item) => item.feature.node)).toEqual(["back"]);
    expect(agentCounts(value)).toEqual({ working: 0, stalled: 1, waiting: 0, idle: 0 });
    expect(seen.tray).toBe("1 agent stalled");
    expect(seen.sidebar).toBe("1 agent stalled");
    expect(seen.trayLive).toBe(false);
    // Still on the tray, after the projects at work; not called idle.
    expect(seen.rows).toEqual(["back · Stalled"]);
  });

  it("counts no working agent when the only live agent is silent past its ETA", () => {
    const value = project([silent(ONE)], 1);
    const seen = surfaces(value);
    expect(seen.board.moving).toHaveLength(0);
    expect(seen.board.stalled[0]!.steps.map((item) => [item.feature.node, item.agent])).toEqual([["late", "silent"]]);
    expect(agentCounts(value)).toEqual({ working: 0, stalled: 1, waiting: 0, idle: 0 });
    expect(seen.tray).toBe("1 agent stalled");
    expect(seen.sidebar).toBe("1 agent stalled");
    expect(seen.rows).toEqual(["late · Stalled"]);
  });

  it("counts an agent with one moving and one stalled step once, as working", () => {
    const value = project([sentBack(ONE), atWork(ONE)], 1);
    const seen = surfaces(value);
    expect(seen.board.moving.map((item) => item.feature.node)).toEqual(["work"]);
    expect(seen.board.stalled[0]!.steps).toHaveLength(1);
    expect(agentCounts(value)).toEqual({ working: 1, stalled: 0, waiting: 0, idle: 0 });
    expect(seen.tray).toBe("1 agent working");
    expect(seen.sidebar).toBe("1 agent working");
    expect(seen.trayLive).toBe(true);
    expect(seen.rows[0]).toContain("work");
  });

  it("names working and stalled agents together and idle ones only alone", () => {
    const mixed = project([atWork(ONE), silent(TWO)], 3);
    expect(agentCounts(mixed)).toEqual({ working: 1, stalled: 1, waiting: 0, idle: 1 });
    expect(surfaces(mixed).sidebar).toBe("1 agent working · 1 agent stalled");
    expect(agentsLine({ working: 0, stalled: 2, waiting: 0, idle: 1 })).toBe("2 agents stalled");
    expect(agentsLine({ working: 0, stalled: 0, waiting: 0, idle: 1 })).toBe("1 agent idle");
    expect(agentsLine({ working: 0, stalled: 0, waiting: 0, idle: 0 })).toBeNull();
  });

  it("counts an agent waiting on its own open question as waiting on the owner, once", () => {
    const asking = (session: string) =>
      step("ask", { liveness: "live", session, claim: { ...HELD, waiting_on: ulid(810) } });
    const value = project([asking(ONE)], 1);
    const seen = surfaces(value);
    expect(seen.board.moving).toHaveLength(0);
    expect(seen.board.stalled).toHaveLength(0);
    expect(agentCounts(value)).toEqual({ working: 0, stalled: 0, waiting: 1, idle: 0 });
    expect(seen.tray).toBe("1 agent waiting on you");
    expect(seen.sidebar).toBe("1 agent waiting on you");
    // A second connected agent holding nothing is the only idle one.
    expect(agentCounts(project([asking(ONE)], 2))).toEqual({ working: 0, stalled: 0, waiting: 1, idle: 1 });
    // Asking on one step and at work on another: working.
    expect(agentCounts(project([asking(ONE), atWork(ONE)], 1))).toEqual({ working: 1, stalled: 0, waiting: 0, idle: 0 });
    // Asking on one step with another stalled: stalled, once.
    expect(agentCounts(project([asking(ONE), silent(ONE)], 1))).toEqual({ working: 0, stalled: 1, waiting: 0, idle: 0 });
  });

  it("orders the tray by working agents and lists a stalled-only project after them", () => {
    const stuck = project([silent(ONE)], 1);
    stuck.summary = { ...stuck.summary, slug: "aaa", title: "aaa" };
    const busy = project([atWork(TWO)], 1);
    busy.summary = { ...busy.summary, slug: "zzz", title: "zzz" };
    const snapshot = snapshotOf(stuck);
    snapshot.state.projects = { aaa: stuck, zzz: busy };
    const { container } = render(
      <ClientContext.Provider value={new FakeClient()}>
        <TrayPopover snapshot={snapshot} canMutate onOpen={vi.fn()} onAnswered={vi.fn()} onRefresh={vi.fn()} />
      </ClientContext.Provider>,
    );
    expect([...container.querySelectorAll(".tray-proj-name")].map((row) => row.textContent)).toEqual(["zzz", "aaa"]);
    expect(container.querySelector(".pop-status")!.textContent).toMatch(/^1 agent working · 1 agent stalled · /);
  });

  it("never names more agents than are connected", () => {
    const cases: [FeatureState[], number][] = [
      [[sentBack(ONE)], 1],
      [[silent(ONE)], 1],
      [[sentBack(ONE), atWork(ONE)], 1],
      [[atWork(ONE), silent(TWO)], 2],
      [[atWork(ONE), silent(TWO)], 5],
      // The summary counts fewer connected agents than hold a stalled step.
      [[silent(ONE), sentBack(TWO)], 1],
      [[silent(ONE)], 0],
      // A stalled step whose agent is gone is on Now, but is no connected agent.
      [[step("gone", { liveness: "quiet", session: ONE, claim: SILENT })], 0],
      [[], 2],
    ];
    for (const [features, active] of cases) {
      const value = project(features, active);
      const counts = agentCounts(value);
      const board = nowBoard(snapshotOf(value));
      const sessions = (list: FeatureState[]) => new Set(list.map((item) => item.session)).size;
      expect(counts.working + counts.stalled + counts.idle).toBeLessThanOrEqual(active);
      expect(counts.working).toBe(sessions(board.moving.map((item) => item.feature)));
      expect(counts.stalled).toBeLessThanOrEqual(
        sessions(board.stalled.flatMap((group) => group.steps.map((item) => item.feature))),
      );
    }
  });

  it("says it in Chinese", () => {
    render(<I18nProvider preference="zh-Hans">{null}</I18nProvider>);
    try {
      expect(agentsLine({ working: 1, stalled: 1, waiting: 0, idle: 2 })).toBe("1 个 agent 在工作 · 1 个 agent 卡住了");
      expect(agentsLine({ working: 0, stalled: 2, waiting: 0, idle: 0 })).toBe("2 个 agent 卡住了");
    } finally {
      render(<I18nProvider preference="en">{null}</I18nProvider>);
    }
  });
});
