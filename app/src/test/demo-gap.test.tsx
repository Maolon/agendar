import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { FeatureState, ProjectState } from "../api/types";
import { DemoGapNote } from "../components/DemoGapNote";
import { Sidebar } from "../components/Sidebar";
import { translator } from "../i18n";
import { demoGap, demoGapLine } from "../state/demoGap";
import { makeDelivery, makeProject, makeSnapshot } from "./fixtures";

/** An event id whose time grows with `n`. */
const at = (n: number) => `01J${n}${"0".repeat(22)}`;

/** `named` is the index of the step the core says the clock runs for. */
function gapProject(gapS: number, steps: Partial<FeatureState>[], named?: number): ProjectState {
  const project = makeProject("alpha");
  const base = project.features["core/one"]!;
  project.features = Object.fromEntries(
    steps.map((step, index) => {
      const node = `core/s${index}`;
      return [node, { ...base, node, ...step }];
    }),
  );
  project.summary.budget = {
    ...project.summary.budget,
    demo_gap_s: gapS,
    ...(named === undefined ? {} : { demo_gap_node: `core/s${named}` }),
  };
  project.deliveries = {};
  return project;
}

describe("demo budget, said in full", () => {
  it("names the step the core records and the command that clears it", () => {
    // The attempt ids say Importer is older: the core's field decides, not they.
    const project = gapProject(
      18_000,
      [
        { title: "Importer", progress: "doing", attempt: at(1) },
        { title: "Search index", progress: "doing", attempt: at(2) },
        { title: "Later", progress: "planned" },
      ],
      1,
    );
    const gap = demoGap(project)!;
    expect(gap.over).toBe(true);
    expect(gap.steps.map((step) => step.title)).toEqual(["Search index", "Importer"]);
    expect(demoGapLine(gap)).toBe(
      "No demo for 5h (limit 4h): Search index and 1 more are started, not delivered. Clears when the agent runs agendar deliver.",
    );
  });

  it("says a review clears it when everything in flight is delivered", () => {
    const project = gapProject(18_000, [{ title: "Search index", progress: "done", attempt: at(1) }]);
    const delivery = makeDelivery({ node: "core/s0" });
    project.deliveries = { [delivery.id]: delivery };
    expect(demoGapLine(demoGap(project)!)).toBe(
      "No demo for 5h (limit 4h): Search index waits for your review. Clears when you review.",
    );
  });

  it("is silent with nothing in flight, and not over below the limit", () => {
    expect(demoGap(gapProject(0, []))).toBeNull();
    expect(demoGap(gapProject(3_600, [{ title: "A", progress: "doing" }], 0))!.over).toBe(false);
  });

  it("names no step when the core records none", () => {
    const gap = demoGap(gapProject(18_000, [{ title: "A", progress: "doing", attempt: at(1) }]))!;
    expect(gap.steps).toEqual([]);
    expect(demoGapLine(gap)).toBe("No demo for 5h (limit 4h). Clears when the agent runs agendar deliver.");
  });

  it("renders the core's reason with the step it names", () => {
    const args = { gap: "5h", limit: "4h", step: "Search index" };
    expect(translator("en")("budget.demo_gap", args)).toBe(
      "No demo for 5h (limit 4h): Search index is started, not delivered.",
    );
    expect(translator("zh-Hans")("budget.demo_gap", args)).toBe(
      "已有 5h 没有演示（上限 4h）：Search index 已开始，未交付。",
    );
    expect(translator("zh-Hant")("budget.demo_gap", args)).toBe(
      "已有 5h 沒有示範（上限 4h）：Search index 已開始，未交付。",
    );
    expect(translator("en")("budget.demo_gap.no_step", args)).toBe("No demo for 5h (limit 4h).");
  });

  it("keeps the command and the word agent in zh-Hans, and words zh-Hant", () => {
    const hans = translator("zh-Hans");
    expect(hans("gap.clear.deliver", { cmd: "agendar deliver" })).toBe("agent 运行 agendar deliver 后清除。");
    expect(hans("gap.what", { d: "5小时", limit: "4小时" })).toBe("已有 5小时 没有演示（上限 4小时）");
    expect(translator("zh-Hant")("gap.clear.deliver", { cmd: "agendar deliver" })).toContain("agendar deliver");
  });

  it("puts one line under the project header only while over the limit", () => {
    const over = gapProject(18_000, [{ title: "Search index", progress: "doing", attempt: at(1) }], 0);
    const { unmount } = render(<DemoGapNote project={over} />);
    const note = screen.getByRole("note");
    expect(note).toHaveTextContent(
      "No demo for 5h (limit 4h): Search index is started, not delivered. Clears when the agent runs agendar deliver.",
    );
    expect(note.querySelector("code")).toHaveTextContent("agendar deliver");
    unmount();
    render(<DemoGapNote project={gapProject(3_600, [{ title: "A", progress: "doing" }], 0)} />);
    expect(screen.queryByRole("note")).toBeNull();
  });

  it("explains the sidebar's No demo chip on hover and to a screen reader", () => {
    const project = gapProject(18_000, [{ title: "Search index", progress: "doing", attempt: at(1) }], 0);
    project.summary.variance = "no_demo";
    project.needs_you = [];
    const snapshot = makeSnapshot();
    snapshot.state.projects = { alpha: project };
    snapshot.needs = [];
    render(
      <Sidebar snapshot={snapshot} selection={null} quietOpen={false} onSelect={vi.fn()} onToggleQuiet={vi.fn()} />,
    );
    const line = "No demo for 5h (limit 4h): Search index is started, not delivered. Clears when the agent runs agendar deliver.";
    expect(screen.getByText("No demo")).toHaveAttribute("title", line);
    expect(document.querySelector("[data-row='project:alpha']")).toHaveAttribute("aria-description", line);
  });
});
