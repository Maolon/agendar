import { render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { formatAgo, formatWorkRange } from "../api/format";
import type { FeatureState, Graph, ProjectState } from "../api/types";
import { ActivityFeed } from "../components/ActivityFeed";
import { isNow, NowCards } from "../components/NowCards";
import { overviewBar, ProjectOverview, stepsByState } from "../components/ProjectOverview";
import { ProjectStatsRow } from "../components/ProjectStatsRow";
import { ProjectTrack } from "../components/Sidebar";
import { nodeDisplay } from "../graph/display";
import { buildLanes, focusWindow, UNSCHEDULED } from "../graph/lanes";
import { LANE_NODEH, LANE_NODEW, layoutLane, MARKER_REACH, markerLabel } from "../graph/layout";
import { makeProject, ulid } from "./fixtures";
import { ASK, planGraph, planProject } from "./graph-fixtures";

const ANCHOR = "2026-02-10T15:00:00.000Z";

function feature(node: string, overrides: Partial<FeatureState> = {}): FeatureState {
  const base = makeProject("alpha").features["core/one"]!;
  return { ...base, node, title: node, depends_on: [], unblocks: [], ...overrides };
}

function graphOf(features: FeatureState[]): Graph {
  return {
    nodes: features.map((item) => ({
      id: `feature:${item.node}`,
      kind: "feature" as const,
      label: item.title,
      feature: item.node,
      state: item.progress,
      assumed: false,
    })),
    edges: features.flatMap((item) =>
      item.depends_on.map((dep) => ({
        from: `feature:${dep}`,
        to: `feature:${item.node}`,
        source_event: ASK,
      })),
    ),
  };
}

describe("plan map lanes", () => {
  const steps = [
    feature("mvp/a", { milestone: "MVP", progress: "done" }),
    feature("mvp/b", { milestone: "MVP", progress: "verified", depends_on: ["mvp/a"] }),
    feature("rel/a", { milestone: "Release", progress: "doing", depends_on: ["mvp/b"] }),
    feature("next/a", { milestone: "Next", depends_on: ["rel/a"] }),
    feature("next/b", { milestone: "Next" }),
    feature("loose", {}),
  ];
  const byNode = Object.fromEntries(steps.map((item) => [item.node, item]));

  it("keeps finished and dropped independent steps out of the open grid", () => {
    const loose = [
      feature("b/doing", { milestone: "B", progress: "doing", gate: "open" }),
      feature("b/todo", { milestone: "B", progress: "planned", gate: "open" }),
      feature("b/done", { milestone: "B", progress: "done", gate: "open" }),
      feature("b/verified", { milestone: "B", progress: "verified", gate: "open" }),
      feature("b/dropped", { milestone: "B", progress: "dropped", gate: "open" }),
    ];
    const features = Object.fromEntries(loose.map((item) => [item.node, item]));
    const lane = buildLanes(graphOf(loose), features).lanes[0]!;
    const window = focusWindow(lane, features);
    expect(window.graph.nodes.map((node) => node.feature).sort()).toEqual(["b/doing", "b/todo"]);
    expect(window.hiddenFinished).toBe(3);
  });

  it("orders lanes by their dependencies and puts unscheduled steps last", () => {
    const plan = buildLanes(graphOf(steps), byNode);
    expect(plan.lanes.map((lane) => lane.key)).toEqual(["MVP", "Release", "Next", UNSCHEDULED]);
    const mvp = plan.lanes[0]!;
    expect(mvp.complete).toBe(true);
    expect(mvp.finished).toBe(2);
    // Lanes draw only their own edges; the cross-lane edge becomes a label.
    expect(plan.lanes[1]!.graph.edges).toEqual([]);
    expect(plan.after.get("feature:rel/a")).toBe("MVP");
    expect(plan.after.get("feature:next/a")).toBe("rel/a");
  });

  it("draws pending and assumed decisions but not answered ones", () => {
    const graph = planGraph();
    const project = planProject();
    const answered = { ...graph.nodes.find((node) => node.kind === "decision")! };
    const drawn = buildLanes(graph, project.features).lanes.flatMap((lane) =>
      lane.graph.nodes.map((node) => node.id),
    );
    expect(drawn).toContain(`decision:${ASK}`);
    const closed: Graph = {
      nodes: graph.nodes.map((node) =>
        node.id === answered.id ? { ...node, need_id: undefined, assumed: false } : node,
      ) as Graph["nodes"],
      edges: graph.edges,
    };
    const closedDrawn = buildLanes(closed, project.features).lanes.flatMap((lane) =>
      lane.graph.nodes.map((node) => node.id),
    );
    expect(closedDrawn).not.toContain(answered.id);
  });

  it("lays independent steps in a grid below the chain, never overlapping or scaled", () => {
    const lane = buildLanes(graphOf(steps), byNode).lanes.find((item) => item.key === "Next")!;
    const extra = [1, 2, 3, 4].map((n) => feature(`next/free-${n}`, { milestone: "Next" }));
    const graph = graphOf([...steps.filter((item) => item.milestone === "Next"), ...extra]);
    const layout = layoutLane(graph, 2 * LANE_NODEW + 14);
    expect(lane.graph.nodes).toHaveLength(2);
    expect(layout.nodeWidth).toBe(LANE_NODEW);
    const boxes = layout.nodes.map(({ position }) => position);
    // Two columns fit the available width; nothing overlaps.
    expect(new Set(boxes.map((box) => box.x)).size).toBe(2);
    for (const [i, a] of boxes.entries()) {
      for (const b of boxes.slice(i + 1)) {
        const apart =
          a.x + LANE_NODEW <= b.x || b.x + LANE_NODEW <= a.x ||
          a.y + LANE_NODEH <= b.y || b.y + LANE_NODEH <= a.y;
        expect(apart).toBe(true);
      }
    }
    expect(layout.width).toBeLessThanOrEqual(2 * LANE_NODEW + 14);
  });
});

describe("long lanes", () => {
  function chain(n: number, progress: (i: number) => FeatureState["progress"]): FeatureState[] {
    return Array.from({ length: n }, (_, i) =>
      feature(`c/${i}`, {
        milestone: "Long",
        progress: progress(i),
        gate: "open",
        liveness: progress(i) === "doing" ? "live" : "quiet",
        depends_on: i ? [`c/${i - 1}`] : [],
      }),
    );
  }
  const lanesOf = (steps: FeatureState[]) =>
    buildLanes(graphOf(steps), Object.fromEntries(steps.map((s) => [s.node, s])));

  it("shows the window around current work and counts what it hides", () => {
    const steps = chain(10, (i) => (i < 6 ? "done" : i === 6 ? "doing" : "planned"));
    const lane = lanesOf(steps).lanes[0]!;
    const window = focusWindow(lane, Object.fromEntries(steps.map((s) => [s.node, s])));
    expect(window.graph.nodes.map((node) => node.feature)).toEqual(["c/5", "c/6", "c/7", "c/8"]);
    expect(window.hiddenBefore).toBe(5);
    expect(window.hiddenAfter).toBe(1);
    expect(window.after.get("feature:c/5")).toBe("5 earlier steps");
  });

  it("puts a finished lane's window on its last steps and leaves short chains whole", () => {
    const done = chain(10, () => "done");
    const window = focusWindow(lanesOf(done).lanes[0]!, Object.fromEntries(done.map((s) => [s.node, s])));
    expect(window.graph.nodes.map((node) => node.feature)).toEqual(["c/7", "c/8", "c/9"]);
    const short = chain(4, () => "planned");
    const whole = focusWindow(lanesOf(short).lanes[0]!, Object.fromEntries(short.map((s) => [s.node, s])));
    expect(whole.hiddenBefore + whole.hiddenAfter).toBe(0);
  });

  it("folds separate chains that are all finished and windows the chain with work", () => {
    const link = (prefix: string, i: number, progress: FeatureState["progress"]) =>
      feature(`${prefix}/${i}`, {
        milestone: "Long",
        progress,
        gate: "open",
        liveness: progress === "doing" ? "live" : "quiet",
        depends_on: i ? [`${prefix}/${i - 1}`] : [],
      });
    const steps = [
      ...Array.from({ length: 8 }, (_, i) => link("a", i, "verified")),
      ...Array.from({ length: 8 }, (_, i) => link("b", i, i === 0 ? "doing" : "planned")),
    ];
    const byNode = Object.fromEntries(steps.map((s) => [s.node, s]));
    const window = focusWindow(lanesOf(steps).lanes[0]!, byNode);
    expect(window.graph.nodes.map((node) => node.feature)).toEqual(["b/0", "b/1", "b/2"]);
    expect(window.hiddenFinished).toBe(8);
    expect(window.hiddenAfter).toBe(5);
    // With every chain finished nothing folds: the window sits on the last steps.
    const finished = steps.map((s) => ({ ...s, progress: "verified" as const }));
    const done = focusWindow(lanesOf(finished).lanes[0]!, Object.fromEntries(finished.map((s) => [s.node, s])));
    expect(done.hiddenFinished).toBe(0);
  });

  it("reads many marker sources as a range and keeps the badge inside the lane", () => {
    expect(markerLabel([5, 2, 4, 3])).toBe("2–5");
    expect(markerLabel([1, 3, 4, 5, 8])).toBe("1·3–5·8");
    expect(markerLabel([1, 2])).toBe("1·2");
    // A chain wraps after four columns; three side steps also feed its fifth step,
    // which opens the second band in the first column.
    const steps = [
      ...chain(5, () => "planned").map((s, i) => (i === 4 ? { ...s, depends_on: ["c/3", "s/0", "s/1", "s/2"] } : s)),
      ...[0, 1, 2].map((i) => feature(`s/${i}`, { milestone: "Long", gate: "open", progress: "planned" })),
    ];
    const layout = layoutLane(graphOf(steps), 4 * LANE_NODEW + 3 * 44);
    const into = layout.markers.find((m) => m.kind === "in" && m.node === "feature:c/4")!;
    expect(into.label).toBe("1–4");
    for (const m of layout.markers) expect(m.x - m.width / 2).toBeGreaterThanOrEqual(-MARKER_REACH);
  });

  it("wraps a long chain into bands inside the available width instead of widening", () => {
    const steps = chain(6, () => "planned");
    const available = 2 * LANE_NODEW + 44;
    const layout = layoutLane(graphOf(steps), available);
    expect(layout.width).toBeLessThanOrEqual(available);
    const ys = [...new Set(layout.nodes.map(({ position }) => position.y))];
    expect(ys).toHaveLength(3);
    // An edge into a new band is a numbered marker pair, never a line around the lane.
    const wrapped = layout.edges.filter((edge) => edge.marker);
    expect(wrapped).toHaveLength(2);
    expect(layout.markers.map((m) => `${m.kind}:${m.label}`)).toEqual(["out:1", "out:2", "in:1", "in:2"]);
  });
});

describe("finished work and the Now section", () => {
  it("never calls a finished step stopped when its session ended", () => {
    const done = feature("a", { progress: "done", liveness: "lost" });
    const node = { id: "feature:a", kind: "feature" as const, label: "a", feature: "a", state: "done", assumed: false };
    expect(nodeDisplay(node, done).sub).toBe("Ready for review");
    const doing = feature("b", { progress: "doing", liveness: "lost" });
    expect(nodeDisplay({ ...node, id: "feature:b", feature: "b" }, doing).sub).toBe("Stopped");
  });

  it("shows only moving steps and needs, naming the harness instead of the host", () => {
    const project: ProjectState = planProject();
    project.features["core/done"] = feature("core/done", { progress: "done", liveness: "lost" });
    expect(isNow(project.features["core/done"]!)).toBe(false);
    render(
      <NowCards
        project={project}
        anchor={ANCHOR}
        needs={[]}
        selected={null}
        onSelect={vi.fn()}
      />,
    );
    const cards = document.querySelectorAll<HTMLElement>(".ncard");
    expect([...cards].map((card) => card.dataset.feat)).toEqual(
      Object.values(project.features)
        .filter(isNow)
        .map((item) => item.node)
        .sort((a, b) => project.features[a]!.title.localeCompare(project.features[b]!.title)),
    );
    expect(document.body.textContent).not.toMatch(/h-[0-9a-z]{6,}/);
  });
});

describe("overview on a long project", () => {
  // Finished steps are named by the order they finished in; titles sort the other way.
  const finished = (i: number) =>
    feature(`done/${i}`, {
      title: `step ${String(100 - i).padStart(3, "0")}`,
      progress: "verified",
      gate: "open",
      attempt: ulid(1000 + i),
    });
  const longProject = (done: number, todo: number) => {
    const steps = [
      ...Array.from({ length: done }, (_, i) => finished(i)),
      feature("work/now", { progress: "doing", gate: "open" }),
      ...Array.from({ length: todo }, (_, i) => feature(`todo/${i}`, { progress: "planned", gate: "open" })),
    ];
    return makeProject("long", { features: Object.fromEntries(steps.map((step) => [step.node, step])) });
  };

  it("folds older finished steps into one bar and keeps the last six as squares", () => {
    const { container } = render(<ProjectOverview project={longProject(85, 1)} />);
    const bar = container.querySelector(".osegs")!;
    expect(bar.className).toBe("osegs folded");
    expect(bar.getAttribute("aria-label")).toBe("85 of 87 steps finished");
    expect(bar.querySelector(".ofold")!.getAttribute("title")).toBe("79 earlier finished steps");
    const squares = [...bar.querySelectorAll(":scope > i")];
    expect(squares.map((square) => square.getAttribute("title"))).toEqual([
      "step 021",
      "step 020",
      "step 019",
      "step 018",
      "step 017",
      "step 016",
      "work/now",
      "todo/0",
    ]);
    expect((bar as HTMLElement).style.getPropertyValue("--osq")).toBe("12px");
  });

  it("keeps one square per step while squares stay readable", () => {
    const steps = stepsByState(Object.values(longProject(85, 1).features));
    expect(overviewBar(steps, 1200).folded).toHaveLength(0);
    expect(overviewBar(steps, 720).folded).toHaveLength(79);
    // Six or fewer finished steps never fold, however long the plan.
    expect(overviewBar(stepsByState(Object.values(longProject(6, 200).features)), 720).folded).toHaveLength(0);
    // A long plan still shares the row: squares shrink below the 12px cap.
    const wide = overviewBar(stepsByState(Object.values(longProject(30, 48).features)), 720);
    expect(wide.folded).toHaveLength(24);
    expect(wide.square).toBeCloseTo((720 * 0.8 + 2) / 55 - 2, 5);
    const { container } = render(<ProjectOverview project={planProject()} />);
    expect(container.querySelector(".ofold")).toBeNull();
  });
});

describe("motion on the project page", () => {
  it("sweeps the radar and breathes the overview only for live work while motion is allowed", () => {
    const project = planProject();
    const { rerender } = render(
      <>
        <ProjectOverview project={project} motion />
        <NowCards project={project} anchor={ANCHOR} needs={[]} selected={null} onSelect={vi.fn()} motion />
      </>,
    );
    // core/menubar is a live, firm, unpaused doing step in the fixture.
    expect(document.querySelector('.ncard[data-feat="core/menubar"] .gd')?.className).toContain("live");
    expect(document.querySelector(".osegs i.doing.live")).not.toBeNull();
    rerender(
      <>
        <ProjectOverview project={project} motion={false} />
        <NowCards project={project} anchor={ANCHOR} needs={[]} selected={null} onSelect={vi.fn()} />
      </>,
    );
    expect(document.querySelector(".live")).toBeNull();
  });
});

describe("a silent agent", () => {
  it("says how long a quiet step has had no word instead of calling it Doing", () => {
    const project = planProject();
    const menubar = project.features["core/menubar"]!;
    const sessionId = "01J00000000000000000000777";
    project.features["core/menubar"] = { ...menubar, liveness: "quiet", session: sessionId };
    project.sessions[sessionId] = {
      id: sessionId,
      host: "h-01h0000000000000000000000",
      harness: "claude_code",
      last_event: "01J00000000000000000000778",
      last_event_ts: "2026-02-10T11:42:00.000Z",
      current_node: "core/menubar",
      liveness: "quiet",
      stalled_on: [],
    };
    render(<NowCards project={project} anchor={ANCHOR} needs={[]} selected={null} onSelect={vi.fn()} motion />);
    const card = document.querySelector<HTMLElement>('.ncard[data-feat="core/menubar"]')!;
    expect(card.querySelector(".nstate")?.textContent).toMatch(/^No word for \d+h/);
    expect(card.querySelector(".gd")?.className).toContain("quiet");
    expect(card.querySelector(".gd")?.className).not.toContain("live");
  });
});

describe("Now card time line", () => {
  it("labels start, the agent's window and now, and says plainly when it runs late", () => {
    const project = planProject();
    const menubar = project.features["core/menubar"]!;
    project.features["core/menubar"] = {
      ...menubar,
      eta: {
        ...menubar.eta!,
        anchor: "2026-02-10T14:00:00.000Z",
        agent_finish: { earliest: "2026-02-10T14:30:00.000Z", latest: "2026-02-10T14:50:00.000Z" },
        paused: false,
      },
    };
    render(<NowCards project={project} anchor={ANCHOR} needs={[]} selected={null} onSelect={vi.fn()} />);
    const card = document.querySelector<HTMLElement>('.ncard[data-feat="core/menubar"]')!;
    expect(card.querySelector(".ntl-win-label")?.textContent).toMatch(/^agent expects /);
    expect(card.querySelector(".ntl-ends")?.textContent).toMatch(/started .*now /);
    const status = card.querySelector(".ntl-status")!;
    expect(status.className).toContain("late");
    expect(status.textContent).toContain("10 min past the expected finish");
  });

  it("says nobody is working on a stopped step instead of on track or late", () => {
    const project = planProject();
    const menubar = project.features["core/menubar"]!;
    for (const finish of [
      { earliest: "2026-02-10T16:30:00.000Z", latest: "2026-02-10T17:50:00.000Z" },
      { earliest: "2026-02-10T14:30:00.000Z", latest: "2026-02-10T14:50:00.000Z" },
    ]) {
      project.features["core/menubar"] = {
        ...menubar,
        liveness: "lost",
        eta: { ...menubar.eta!, anchor: "2026-02-10T14:00:00.000Z", agent_finish: finish, paused: false },
      };
      const { unmount } = render(
        <NowCards project={project} anchor={ANCHOR} needs={[]} selected={null} onSelect={vi.fn()} />,
      );
      const status = document.querySelector<HTMLElement>('.ncard[data-feat="core/menubar"] .ntl-status')!;
      expect(status.textContent).toContain("Nobody is working on it");
      expect(status.textContent).not.toMatch(/On track|past the expected finish/);
      expect(status.className).not.toContain("late");
      unmount();
    }
  });
});

describe("header statistics", () => {
  it("says what each number counts and never invents tokens", () => {
    render(
      <ProjectStatsRow
        stats={{
          agents_running: 2,
          agents_idle: 0,
          agents_stalled: 1,
          finished_24h: 3,
          agent_ms_24h: 130 * 60000,
          median_step_ms: 34 * 60000,
          timed_steps: 3,
          instant_excluded: 12,
          
        }}
      />,
    );
    const stat = (key: string) => document.querySelector<HTMLElement>(`[data-stat="${key}"]`)!.textContent;
    expect(stat("agents")).toContain("2");
    expect(stat("agents")).toContain("1 waiting on you");
    expect(stat("time")).toContain("2h 10m");
    expect(stat("median")).toContain("34 min");
    expect(stat("median")).toContain("of 3 steps");
    expect(stat("tokens")).toContain("—");
    expect(stat("tokens")).toContain("no agent logs found");
  });

  it("shows tokens from the agents' logs, compactly", () => {
    render(
      <ProjectStatsRow
        stats={{
          agents_running: 1,
          agents_idle: 0,
          agents_stalled: 0,
          finished_24h: 1,
          agent_ms_24h: 60000,
          timed_steps: 1,
          instant_excluded: 0,
          tokens: { input: 120_000, cached: 3_400_000, output: 52_000 },
        }}
      />,
    );
    const tile = document.querySelector<HTMLElement>('[data-stat="tokens"]')!;
    expect(tile.textContent).toContain("3.6M");
    // The tile keeps one short note so it never truncates; hover has the rest.
    expect(tile.querySelector(".pstat-n")?.textContent).toBe("3.4M cached");
    expect(tile.getAttribute("title")).toContain("3.4M cached · 52K out");
  });
});

describe("activity and copy", () => {
  it("renders activity sentences newest first and folds the rest", () => {
    const items = Array.from({ length: 8 }, (_, i) => ({
      ts: `2026-02-10T14:0${i}:00.000Z`,
      actor: "Claude Code",
      actor_kind: "agent" as const,
      icon: "check",
      text: `Claude Code finished step ${i}`,
      detail: null,
      node: null,
      count: 1,
    }));
    render(<ActivityFeed items={items} />);
    expect(screen.getAllByRole("listitem")).toHaveLength(6);
    expect(screen.getByRole("button", { name: "Show all 8" })).toBeInTheDocument();
  });

  it("forecasts the whole project in the header, as a clock only while someone works", () => {
    const project = makeProject("alpha");
    const forecast = {
      open_steps: 6,
      agents: 1,
      work_ms: [2 * 3_600_000, 4 * 3_600_000] as [number, number],
      finish: { earliest: "2026-02-10T17:00:00.000Z", latest: "2026-02-10T19:00:00.000Z" },
      step_ms: [600_000, 1_800_000] as [number, number],
      basis_steps: 9,
    };
    project.stats = {
      agents_running: 1,
      agents_idle: 0,
      agents_stalled: 0,
      finished_24h: 9,
      agent_ms_24h: 1,
      timed_steps: 9,
      instant_excluded: 0,
      forecast,
    };
    const { container, rerender } = render(<ProjectOverview project={project} />);
    const line = container.querySelector(".oforecast")!;
    expect(line.textContent).toMatch(/project done ~\d\d:\d\d–\d\d:\d\d/);
    expect(line.getAttribute("title")).toContain("6 open steps");
    expect(line.getAttribute("title")).toContain("10–30 min each");

    const { finish: _finish, ...idle } = forecast;
    project.stats = { ...project.stats, forecast: { ...idle, agents: 0 } };
    rerender(<ProjectOverview project={structuredClone(project)} />);
    expect(container.querySelector(".oforecast")!.textContent).toBe(" · 2–4h of work left, nobody on it");
  });

  it("formats work as minutes, then half hours", () => {
    expect(formatWorkRange([20 * 60_000, 45 * 60_000])).toBe("20–45 min");
    expect(formatWorkRange([90 * 60_000, 4 * 3_600_000])).toBe("1.5–4h");
    expect(formatWorkRange([3 * 3_600_000, 3 * 3_600_000])).toBe("about 3h");
  });

  it("never reads just now ago", () => {
    expect(formatAgo(ANCHOR, ANCHOR)).toBe("just now");
    expect(formatAgo("2026-02-10T14:55:00.000Z", ANCHOR)).toBe("5m ago");
  });

  it("keeps the sidebar strip a fixed width when a project has many steps", () => {
    const many = Object.fromEntries(
      Array.from({ length: 26 }, (_, i) => [`s/${i}`, feature(`s/${i}`, { progress: i < 13 ? "done" : "planned", gate: "open" })]),
    );
    const { container } = render(<ProjectTrack features={many} />);
    const strip = container.querySelector(".track")!;
    expect(strip.className).toContain("dense");
    const segments = within(strip as HTMLElement).queryAllByRole("presentation");
    expect(segments).toHaveLength(0);
    // One block per state, sized by count: finished leads, the rest is outlined.
    const blocks = strip.querySelectorAll("i");
    expect(blocks).toHaveLength(2);
    expect(blocks[0]!.className).toBe("d");
    expect(blocks[0]!.getAttribute("data-count")).toBe("13");
    expect(blocks[1]!.className).toBe("p");
    expect(blocks[1]!.getAttribute("data-count")).toBe("13");
  });
});
