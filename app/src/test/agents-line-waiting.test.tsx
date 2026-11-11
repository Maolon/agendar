import { render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ClientContext } from "../api/store";
import type { ClaimState, FeatureState, ProjectState, Snapshot } from "../api/types";
import { Sidebar } from "../components/Sidebar";
import { TrayPopover } from "../components/TrayPopover";
import { I18nProvider, type LanguagePreference } from "../i18n";
import { agentCounts, agentsLine } from "../state/project";
import { FakeClient, makeProject, makeSnapshot, ulid } from "./fixtures";

const AT = "2026-02-10T14:00:00.000Z";
const HELD: ClaimState = { state: "held", since: AT, last_word: AT };
const ONE = ulid(821);
const TWO = ulid(822);

function step(node: string, overrides: Partial<FeatureState>): FeatureState {
  const base = makeProject("alpha").features["core/one"]!;
  return { ...base, node, title: node, depends_on: [], unblocks: [], gate: "open", progress: "doing", ...overrides };
}

/** In progress, live, held because the agent waits on its own open question. */
const asking = (session: string, node = "ask") =>
  step(node, { liveness: "live", session, claim: { ...HELD, waiting_on: ulid(830) } });
const atWork = (session: string) => step("work", { liveness: "live", session, claim: HELD });
const silent = (session: string) => step("late", { liveness: "live", session, claim: { ...HELD, state: "stalled" } });

function project(features: FeatureState[], active: number, slug = "alpha"): ProjectState {
  const value = makeProject(slug, { features: Object.fromEntries(features.map((item) => [item.node, item])) });
  value.summary = { ...value.summary, slug, title: slug, active_agents: active, needs_count: 0 };
  value.needs_you = [];
  return value;
}

function snapshotOf(...projects: ProjectState[]): Snapshot {
  const snapshot = makeSnapshot({ withRescue: false });
  snapshot.state.projects = Object.fromEntries(projects.map((value) => [value.summary.slug, value]));
  snapshot.needs = [];
  snapshot.brief.delivered = [];
  return snapshot;
}

function tray(snapshot: Snapshot) {
  const { container, unmount } = render(
    <ClientContext.Provider value={new FakeClient()}>
      <TrayPopover snapshot={snapshot} canMutate onOpen={vi.fn()} onAnswered={vi.fn()} onRefresh={vi.fn()} />
    </ClientContext.Provider>,
  );
  const seen = {
    status: container.querySelector(".pop-status")!.textContent!.split(" · ").slice(0, -1).join(" · "),
    names: [...container.querySelectorAll(".tray-proj-name")].map((row) => row.textContent),
    lines: [...container.querySelectorAll(".tray-proj-line")].map((row) => row.textContent),
    quiet: container.querySelector(".pop-quiet") !== null,
  };
  unmount();
  return seen;
}

function sidebar(snapshot: Snapshot): string {
  const { container, unmount } = render(
    <Sidebar snapshot={snapshot} selection={null} quietOpen={false} onSelect={vi.fn()} onToggleQuiet={vi.fn()} />,
  );
  const text = container.querySelector(".foot-agents .foot-text")!.textContent!;
  unmount();
  return text;
}

function inLanguage<T>(preference: LanguagePreference, run: () => T): T {
  render(<I18nProvider preference={preference}>{null}</I18nProvider>);
  try {
    return run();
  } finally {
    render(<I18nProvider preference="en">{null}</I18nProvider>);
  }
}

describe("an agent waiting on its own question is counted as waiting on the owner", () => {
  it("names the only agent as waiting on you, on the tray and the sidebar", () => {
    const value = project([asking(ONE)], 1);
    expect(agentCounts(value)).toEqual({ working: 0, stalled: 0, waiting: 1, idle: 0 });
    const seen = tray(snapshotOf(value));
    expect(seen.status).toBe("1 agent waiting on you");
    expect(seen.quiet).toBe(false);
    expect(seen.lines).toEqual(["1 agent waiting on you"]);
    expect(sidebar(snapshotOf(value))).toBe("1 agent waiting on you");
  });

  it("lists the project with those that need the owner, before working and idle ones", () => {
    const idle = project([], 1, "aaa");
    const busy = project([atWork(TWO)], 1, "bbb");
    const waits = project([asking(ONE)], 1, "zzz");
    const seen = tray(snapshotOf(idle, busy, waits));
    expect(seen.names).toEqual(["zzz", "bbb", "aaa"]);
    expect(seen.lines[0]).toBe("1 agent waiting on you");
    expect(seen.lines[2]).toMatch(/^Idle/);
    expect(seen.status).toBe("1 agent working · 1 agent waiting on you");
  });

  it("says it in three languages", () => {
    const one = { working: 0, stalled: 0, waiting: 1, idle: 0 };
    const mixed = { working: 1, stalled: 2, waiting: 2, idle: 1 };
    expect(agentsLine(one)).toBe("1 agent waiting on you");
    expect(agentsLine(mixed)).toBe("1 agent working · 2 agents waiting on you · 2 agents stalled");
    inLanguage("zh-Hans", () => {
      expect(agentsLine(one)).toBe("1 个 agent 在等你");
      expect(agentsLine(mixed)).toBe("1 个 agent 在工作 · 2 个 agent 在等你 · 2 个 agent 卡住了");
      expect(tray(snapshotOf(project([asking(ONE)], 1))).lines).toEqual(["1 个 agent 在等你"]);
    });
    inLanguage("zh-Hant", () => {
      expect(agentsLine(one)).toBe("1 個 AI 代理在等你");
      expect(agentsLine(mixed)).toBe("1 個 AI 代理運作中 · 2 個 AI 代理在等你 · 2 個 AI 代理卡住了");
    });
  });

  it("names one working and one waiting", () => {
    const value = project([atWork(ONE), asking(TWO)], 2);
    expect(agentCounts(value)).toEqual({ working: 1, stalled: 0, waiting: 1, idle: 0 });
    expect(sidebar(snapshotOf(value))).toBe("1 agent working · 1 agent waiting on you");
  });

  it("names waiting before stalled", () => {
    const value = project([silent(ONE), asking(TWO)], 2);
    expect(agentCounts(value)).toEqual({ working: 0, stalled: 1, waiting: 1, idle: 0 });
    expect(sidebar(snapshotOf(value))).toBe("1 agent waiting on you · 1 agent stalled");
    expect(tray(snapshotOf(value)).lines).toEqual(["1 agent waiting on you"]);
  });

  it("still says no agents are connected when none is", () => {
    for (const value of [project([], 0), project([asking(ONE)], 0), project([step("ask", { ...asking(ONE), liveness: "quiet" })], 0)]) {
      expect(agentCounts(value)).toEqual({ working: 0, stalled: 0, waiting: 0, idle: 0 });
      expect(sidebar(snapshotOf(value))).toBe("No agents connected");
      expect(tray(snapshotOf(value)).status).toBe("No agents connected");
    }
  });

  it("never counts more agents than are connected", () => {
    const cases: [FeatureState[], number][] = [
      [[asking(ONE)], 1],
      [[asking(ONE), asking(ONE, "ask2")], 1],
      [[asking(ONE), asking(TWO, "ask2")], 1],
      [[asking(ONE), asking(TWO, "ask2")], 2],
      [[asking(ONE), silent(TWO)], 1],
      [[asking(ONE), atWork(TWO)], 2],
      [[asking(ONE), atWork(TWO)], 1],
      [[asking(ONE), atWork(ONE)], 1],
      [[asking(ONE), silent(ONE)], 1],
      [[asking(ONE)], 4],
    ];
    for (const [features, active] of cases) {
      const counts = agentCounts(project(features, active));
      expect(Object.values(counts).every((n) => n >= 0)).toBe(true);
      expect(counts.working + counts.stalled + counts.waiting + counts.idle).toBeLessThanOrEqual(active);
    }
  });
});
