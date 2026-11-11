import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ClientContext } from "../api/store";
import type { FeatureState, Graph, ProjectState } from "../api/types";
import { PlanMap } from "../components/PlanMap";
import { FakeClient, makeProject } from "./fixtures";

function feature(node: string, overrides: Partial<FeatureState> = {}): FeatureState {
  const base = makeProject("alpha").features["core/one"]!;
  return { ...base, node, title: node, depends_on: [], unblocks: [], progress: "planned", milestone: "Beta", ...overrides };
}

function setup(canMutate = true) {
  const features = {
    "a/live": feature("a/live"),
    "a/old": feature("a/old", { untouched: true, priority: 2, field_revisions: { priority: "p-rev" } }),
    "b/old": feature("b/old", { untouched: true, milestone: "Later" }),
  };
  const project: ProjectState = { ...makeProject("alpha"), features };
  const graph: Graph = {
    nodes: Object.keys(features).map((node) => ({
      id: `feature:${node}`,
      kind: "feature" as const,
      label: node,
      feature: node,
      state: "planned",
      assumed: false,
    })),
    edges: [],
  };
  const client = new FakeClient();
  const update = vi.spyOn(client, "updateFeature");
  const onRefresh = vi.fn();
  const onSelect = vi.fn();
  const view = render(
    <ClientContext.Provider value={client}>
      <PlanMap
        graph={graph}
        project={project}
        selected={null}
        onSelect={onSelect}
        onOpenNeed={() => {}}
        motion={false}
        pulses={[]}
        pulseToken={0}
        play={null}
        onPlayed={() => {}}
        canMutate={canMutate}
        onRefresh={onRefresh}
      />
    </ClientContext.Provider>,
  );
  return { ...view, update, onRefresh, onSelect };
}

describe("Untouched steps on the plan", () => {
  it("fold into the last lane with their count, and lane counts still include them", () => {
    const { container } = setup();
    const lanes = [...container.querySelectorAll("section.plane")];
    const last = lanes[lanes.length - 1]!;
    expect(last.getAttribute("data-testid")).toBe("untouched");
    expect(last.querySelector(".plane-count")?.textContent).toBe("2");
    expect(last.querySelector(".plane-h")?.getAttribute("aria-expanded")).toBe("false");
    expect(container.querySelector("[data-untouched]")).toBeNull();
    // Beta still counts both its steps; only the live one is drawn.
    const beta = container.querySelector('[data-lane="Beta"]')!;
    expect(beta.querySelector(".plane-count")?.textContent).toBe("0/2");
    expect(beta.textContent).toContain("a/live");
    expect(beta.textContent).not.toContain("a/old");
    // Later has nothing to draw: it stays a row of squares.
    expect(container.querySelector('[data-lane="Later"] .plane-squares')).not.toBeNull();
  });

  it("Keep patches the step's own priority and refreshes", async () => {
    const { container, update, onRefresh } = setup();
    fireEvent.click(screen.getByTestId("untouched").querySelector(".plane-h")!);
    const row = container.querySelector('[data-untouched="a/old"]')!;
    fireEvent.click(row.querySelector("button.link")!);
    await waitFor(() => expect(onRefresh).toHaveBeenCalled());
    expect(update).toHaveBeenCalledWith(
      expect.objectContaining({ node: "a/old", patch: { priority: 2 }, expected_fields: { priority: "p-rev" } }),
    );
  });

  it("Keep is disabled when the app cannot write", () => {
    const { container } = setup(false);
    fireEvent.click(screen.getByTestId("untouched").querySelector(".plane-h")!);
    expect(container.querySelector<HTMLButtonElement>('[data-untouched="a/old"] button.link')!.disabled).toBe(true);
  });
});
