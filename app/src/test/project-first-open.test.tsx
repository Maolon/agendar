import { act, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ClientContext } from "../api/store";
import type { FeatureState, Graph, ProjectState } from "../api/types";
import { Project } from "../views/Project";
import { ASK_ONE, FakeClient, makeProject, projectProps } from "./fixtures";

const MILESTONES = 19;
const STEPS = 190;
/** The first lanes are finished; the rest hold a chain with the last steps still open. */
const FINISHED_LANES = 13;

/** A long plan: 190 steps in 19 milestones, each a chain of ten, most of it finished. */
function longPlan(): { project: ProjectState; graph: Graph } {
  const project = makeProject("alpha");
  const base = project.features["core/one"]!;
  const features: Record<string, FeatureState> = {};
  for (let index = 0; index < STEPS; index += 1) {
    const lane = index % MILESTONES;
    const place = Math.floor(index / MILESTONES);
    const node = `m${String(lane).padStart(2, "0")}/s${place}`;
    const open = lane >= FINISHED_LANES && place >= 7;
    features[node] = {
      ...base,
      node,
      title: `Step ${place} of milestone ${lane}`,
      milestone: `Milestone ${String(lane).padStart(2, "0")}`,
      depends_on: place ? [`m${String(lane).padStart(2, "0")}/s${place - 1}`] : [],
      unblocks: [],
      progress: open ? (place === 7 ? "doing" : "planned") : "verified",
      gate: "open",
    };
  }
  project.features = features;
  const steps = Object.values(features);
  const graph: Graph = {
    nodes: steps.map((step) => ({
      id: `feature:${step.node}`,
      kind: "feature" as const,
      label: step.title,
      feature: step.node,
      state: step.progress,
      assumed: false,
    })),
    edges: steps.flatMap((step) =>
      step.depends_on.map((upstream) => ({
        from: `feature:${upstream}`,
        to: `feature:${step.node}`,
        source_event: ASK_ONE,
      })),
    ),
  };
  return { project, graph };
}

/** Each lane in page order with its count and the room its drawing takes. */
function laneRooms(container: HTMLElement): string[] {
  return [...container.querySelectorAll<HTMLElement>("[data-lane]")].map((lane) => {
    const stage = lane.querySelector<HTMLElement>(".gstage");
    const count = lane.querySelector(".plane-count")?.textContent ?? "";
    return `${lane.dataset.lane} ${count} ${stage ? `${stage.style.width} x ${stage.style.height}` : "folded"}`;
  });
}

describe("first open of a long project", () => {
  it("shows the plan's lanes at once and keeps their room until the graph answers", async () => {
    const { project, graph } = longPlan();
    const client = new FakeClient();
    let answer!: () => void;
    const answered = new Promise<void>((resolve) => (answer = resolve));
    client.getProject = async () => {
      await answered;
      return { project, graph };
    };
    const page = (
      <ClientContext.Provider value={client}>
        <Project {...projectProps(project, client)} />
      </ClientContext.Provider>
    );

    const first = render(page);
    // Before `get_project` answers: the header, every lane with its count,
    // and no waiting text.
    expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent(project.summary.title);
    expect(first.container).not.toHaveTextContent(/Loading/);
    const before = laneRooms(first.container);
    expect(before).toHaveLength(MILESTONES);
    expect(before.filter((lane) => lane.endsWith("folded"))).toHaveLength(FINISHED_LANES);
    expect(before[0]).toBe("Milestone 00 10/10 folded");
    expect(first.container.querySelectorAll("[data-graph-node]")).toHaveLength(0);
    // Finished lanes are rows of squares: the first render stays small.
    expect(first.container.querySelectorAll("*").length).toBeLessThan(1500);

    await act(async () => {
      answer();
      await answered;
    });
    // The drawing fills the room that was kept: no lane moves or resizes.
    expect(first.container.querySelectorAll("[data-graph-node]").length).toBeGreaterThan(0);
    expect(laneRooms(first.container)).toEqual(before);
    // Only the open lanes' windows are drawn, not the 190 steps.
    expect(first.container.querySelectorAll("[data-graph-node]").length).toBeLessThan(40);
    first.unmount();

    // Opened again, the plan is drawn in the first render.
    const again = render(page);
    expect(again.container.querySelectorAll("[data-graph-node]").length).toBeGreaterThan(0);
    expect(laneRooms(again.container)).toEqual(before);
  });
});
