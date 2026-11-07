import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { formatFinishWindow } from "../api/format";
import type { FeatureState, ProjectState, Snapshot } from "../api/types";
import { Sidebar, orderedProjects } from "../components/Sidebar";
import { ClientContext } from "../api/store";
import { ArchiveToggle } from "../components/Agenda";
import { FakeClient, makeProject, makeSnapshot } from "./fixtures";

const ANCHOR_START = "2026-02-10T14:00:00.000Z";

function feature(overrides: Partial<FeatureState>): FeatureState {
  return {
    ...makeProject("x").features["core/one"]!,
    ...overrides,
  };
}

function project(
  slug: string,
  variance: ProjectState["summary"]["variance"],
  options: { features?: Record<string, FeatureState>; active?: number } = {},
): ProjectState {
  const base = makeProject(slug);
  return {
    ...base,
    summary: {
      ...base.summary,
      variance,
      active_agents: options.active ?? 0,
      stalled_agents: 0,
      needs_count: 0,
      feature_counts: { doing: 1 },
    },
    features: options.features ?? {},
    needs_you: [],
  };
}

function snapshotWith(projects: ProjectState[]): Snapshot {
  const snapshot = makeSnapshot();
  snapshot.state.projects = Object.fromEntries(projects.map((item) => [item.summary.slug, item]));
  snapshot.needs = [];
  return snapshot;
}

function renderSidebar(snapshot: Snapshot, quietOpen = false) {
  const onToggleQuiet = vi.fn();
  render(
    <Sidebar
      snapshot={snapshot}
      selection={null}
      filter=""
      quietOpen={quietOpen}
      onSelect={vi.fn()}
      onToggleQuiet={onToggleQuiet}
    />,
  );
  return { onToggleQuiet };
}

describe("sidebar ordering and rows", () => {
  it("orders by variance first (rework, stopped, no demo, active) and collapses quiet", () => {
    const running = feature({
      progress: "doing",
      gate: "open",
      liveness: "live",
      eta: {
        agent_range: { min_s: 3600, max_s: 7200 },
        anchor: ANCHOR_START,
        paused_s: 0,
        agent_finish: {
          earliest: "2026-02-10T15:30:00.000Z",
          latest: "2026-02-10T16:00:00.000Z",
        },
        late_by_s: 0,
        paused: false,
      },
    });
    const snapshot = snapshotWith([
      project("active-one", "active", { features: { "core/one": running }, active: 1 }),
      project("quiet-one", "quiet"),
      project("nodemo-one", "no_demo"),
      project("rework-one", "rework"),
      project("stopped-one", "stopped"),
      project("active-two", "active"),
    ]);
    // The row shows when the whole project finishes, not the step in progress.
    snapshot.state.projects["active-one"]!.stats = {
      agents_running: 1,
      agents_idle: 0,
      agents_stalled: 0,
      finished_24h: 4,
      agent_ms_24h: 3_600_000,
      timed_steps: 4,
      instant_excluded: 0,
      forecast: {
        open_steps: 5,
        agents: 1,
        work_ms: [7_200_000, 14_400_000],
        finish: { earliest: "2026-02-10T17:00:00.000Z", latest: "2026-02-10T19:00:00.000Z" },
        step_ms: [1_200_000, 3_000_000],
        basis_steps: 4,
      },
    };

    renderSidebar(snapshot);

    const rows = Array.from(document.querySelectorAll<HTMLElement>("[data-row^='project:']")).map(
      (row) => row.dataset.row,
    );
    expect(rows).toEqual([
      "project:rework-one",
      "project:stopped-one",
      "project:nodemo-one",
      "project:active-one",
      "project:active-two",
    ]);
    expect(screen.queryByText("quiet-one")).toBeNull();
    expect(screen.getByRole("button", { name: "1 more quiet" })).toBeInTheDocument();
    expect(
      screen.getByText(
        formatFinishWindow({
          earliest: "2026-02-10T17:00:00.000Z",
          latest: "2026-02-10T19:00:00.000Z",
        }),
      ),
    ).toBeInTheDocument();
    expect(
      screen.queryByText(
        formatFinishWindow({
          earliest: "2026-02-10T15:30:00.000Z",
          latest: "2026-02-10T15:30:00.000Z",
        }),
      ),
    ).toBeNull();
  });

  it("expands quiet projects without changing the collapse preference", async () => {
    const snapshot = snapshotWith([
      project("active-one", "active"),
      project("quiet-one", "quiet"),
    ]);
    const { onToggleQuiet } = renderSidebar(snapshot);

    await userEvent.click(screen.getByRole("button", { name: "1 more quiet" }));
    expect(onToggleQuiet).toHaveBeenCalled();
  });

  it("shows a needs-row reason and a paused project label", () => {
    const paused = feature({
      progress: "doing",
      gate: "open",
      paused: true,
      liveness: "quiet",
    });
    const snapshot = snapshotWith([
      project("paused-one", "active", { features: { "core/one": paused }, active: 0 }),
    ]);
    const base = makeSnapshot();
    snapshot.needs = base.needs.slice(0, 1);
    snapshot.needs[0]!.reason = "2 agents waiting · 38m";

    renderSidebar(snapshot);

    expect(screen.getByText("2 agents waiting · 38m")).toBeInTheDocument();
    expect(screen.getByText("paused")).toBeInTheDocument();
  });

  it("keeps a review queue visible with zero stalled agents", () => {
    const snapshot = makeSnapshot();
    snapshot.needs = [
      {
        ...makeSnapshot().needs[1]!,
        id: "need-review",
        kind: "review",
        stalled_agents: 0,
        reason: "Ready for review · 1h",
      },
    ];
    snapshot.needs[0]!.project = "alpha";
    snapshot.needs[0]!.source = "review:one";
    renderSidebar(snapshot);
    expect(screen.getByText("Ready for review · 1h")).toBeInTheDocument();
    expect(document.querySelector('[data-row="need:need-review"]')).not.toBeNull();
  });

  it("orders projects by variance then slug for the same variance", () => {
    const snapshot = snapshotWith([
      project("zeta", "active"),
      project("alpha", "active"),
      project("beta", "stopped"),
    ]);
    expect(orderedProjects(snapshot).map((summary) => summary.slug)).toEqual([
      "beta",
      "alpha",
      "zeta",
    ]);
  });

  it("puts the same-variance project with a need first when only one has one", () => {
    const snapshot = snapshotWith([
      project("alpha", "active"),
      project("zeta", "active"),
    ]);
    snapshot.needs = [
      {
        ...makeSnapshot().needs[0]!,
        id: "need-zeta",
        project: "zeta",
        since: "2026-02-10T14:22:00.000Z",
      },
    ];
    expect(orderedProjects(snapshot).map((summary) => summary.slug)).toEqual([
      "zeta",
      "alpha",
    ]);
  });
});

describe("motion", () => {
  it("keeps no Motion toggle in the sidebar; it moved to Settings", () => {
    render(
      <Sidebar
        snapshot={snapshotWith([project("alpha", "active")])}
        selection={null}
        filter=""
        quietOpen={false}
        onSelect={vi.fn()}
        onToggleQuiet={vi.fn()}
      />,
    );
    expect(screen.queryByRole("button", { name: /Motion/ })).toBeNull();
  });

  it("opens Settings from the footer, where the Motion setting lives", async () => {
    const onOpenSettings = vi.fn();
    render(
      <Sidebar
        snapshot={snapshotWith([project("alpha", "active")])}
        selection={null}
        filter=""
        quietOpen={false}
        onSelect={vi.fn()}
        onToggleQuiet={vi.fn()}
        onOpenSettings={onOpenSettings}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(onOpenSettings).toHaveBeenCalledTimes(1);
  });
});

describe("archiving", () => {
  it("keeps archived projects reachable under their own toggle", async () => {
    const user = userEvent.setup();
    const snapshot = snapshotWith([project("live-one", "active"), project("old-one", "quiet")]);
    snapshot.state.projects["old-one"]!.summary.archived = true;
    renderSidebar(snapshot);
    expect(document.querySelector("[data-row='project:old-one']")).toBeNull();
    await user.click(screen.getByRole("button", { name: "1 archived" }));
    expect(document.querySelector("[data-row='project:old-one']")).not.toBeNull();
  });

  it("archives and restores through update_project with the field guard", async () => {
    const user = userEvent.setup();
    const client = new FakeClient();
    const project = makeProject("alpha");
    project.field_revisions = { ...project.field_revisions, archived: "a".repeat(64) };
    const { rerender } = render(
      <ClientContext.Provider value={client}>
        <ArchiveToggle project={project} canMutate />
      </ClientContext.Provider>,
    );
    await user.click(screen.getByRole("button", { name: "Archive" }));
    const call = client.calls.find((entry) => entry.command === "update_project");
    expect(call?.request).toMatchObject({
      project: "alpha",
      patch: { archived: true },
      expected_fields: { archived: "a".repeat(64) },
    });
    project.summary.archived = true;
    rerender(
      <ClientContext.Provider value={client}>
        <ArchiveToggle project={structuredClone(project)} canMutate />
      </ClientContext.Provider>,
    );
    expect(screen.getByRole("button", { name: "Restore" })).toBeInTheDocument();
  });
});

describe("narrow sidebar finish time", () => {
  it("keeps only the finish of a window", async () => {
    const { shortFinish } = await import("../components/Sidebar");
    expect(shortFinish("~10:54–11:51")).toBe("~11:51");
    expect(shortFinish("10:54–11:51")).toBe("11:51");
    expect(shortFinish("paused")).toBe("paused");
    expect(shortFinish("1.5h left")).toBe("1.5h");
    expect(shortFinish("1–2h left")).toBe("1–2h");
  });
});
