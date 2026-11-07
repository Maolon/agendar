import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { formatRemaining } from "../api/format";
import { ClientContext } from "../api/store";
import type { BriefRow, Snapshot } from "../api/types";
import { BudgetDots } from "../components/BriefVisual";
import { Brief } from "../views/Brief";
import {
  ASK_ONE,
  FakeClient,
  easyCard,
  makeDecision,
  makeDeliverySnapshot,
  makeGraph,
  makeNeed,
  makeSnapshot,
  textBlob,
  ulid,
} from "./fixtures";

function withDelivered(snapshot: Snapshot): Snapshot {
  const row: BriefRow = {
    id: "delivered:review",
    text: "bookfinder: search finds books by author alias",
    detail: "Needs your review",
    target: { kind: "need", id: "review-one" },
    source_ids: [],
  };
  snapshot.brief.delivered = [row];
  return snapshot;
}

describe("brief sections", () => {
  it("renders all six section labels with their projected rows", () => {
    const client = new FakeClient(withDelivered(makeDeliverySnapshot()));
    render(
      <ClientContext.Provider value={client}>
        <Brief snapshot={client.snapshot} onSelect={vi.fn()} />
      </ClientContext.Provider>,
    );

    for (const label of [
      "DELIVERED",
      "COMING UP",
      "NEEDS YOU",
      "IF YOU WAIT",
      "BY DEFAULT",
      "AT RISK",
    ]) {
      expect(screen.getAllByText(label).length).toBeGreaterThan(0);
    }
    expect(screen.getByText("bookfinder: search finds books by author alias")).toBeInTheDocument();
    expect(screen.getByText("alpha: CLI ask likely done ~16:00")).toBeInTheDocument();
  });

  it("counts the reachable blocked steps from the graph in If you wait", async () => {
    const client = new FakeClient(makeSnapshot());
    client.graph = makeGraph({
      nodes: [
        {
          id: `decision:${ASK_ONE}`,
          kind: "decision",
          label: "How should answers reach idle agents?",
          need_id: "need-one",
          state: "open",
          assumed: false,
        },
        {
          id: "feature:core/two",
          kind: "feature",
          label: "Core two",
          feature: "core/two",
          state: "planned",
          assumed: false,
        },
        {
          id: "feature:core/three",
          kind: "feature",
          label: "Core three",
          feature: "core/three",
          state: "planned",
          assumed: false,
        },
      ],
      edges: [
        { from: `decision:${ASK_ONE}`, to: "feature:core/two", source_event: ASK_ONE },
        { from: "feature:core/two", to: "feature:core/three", source_event: ulid(180) },
      ],
    });
    render(
      <ClientContext.Provider value={client}>
        <Brief snapshot={client.snapshot} onSelect={vi.fn()} />
      </ClientContext.Provider>,
    );
    // The row's projected text names one step; the graph reaches two.
    expect(
      await screen.findByText("alpha: Core two and 1 more step stays blocked"),
    ).toBeInTheDocument();
    expect(screen.queryByText("alpha: core/two stays blocked")).not.toBeInTheDocument();
  });

  it("resolves a need-targeted If you wait row by its own need, not the first in the project", async () => {
    const snapshot = makeSnapshot();
    // A higher-ranked rescue in the same project must not replace this row's
    // decision with its own (unrelated) graph reach.
    snapshot.needs.unshift(
      makeNeed({
        id: "rescue-one",
        kind: "rescue",
        project: "alpha",
        source: ulid(190),
        question: "Agent stopped mid-task",
        node: "core/one",
      }),
    );
    snapshot.brief.if_you_wait = [
      {
        id: "wait",
        text: "alpha: core/two stays blocked",
        target: { kind: "need", id: "need-one" },
        source_ids: [ASK_ONE],
      },
    ];
    const client = new FakeClient(snapshot);
    client.graph = makeGraph({
      nodes: [
        {
          id: `decision:${ASK_ONE}`,
          kind: "decision",
          label: "How should answers reach idle agents?",
          need_id: "need-one",
          state: "open",
          assumed: false,
        },
        {
          id: "feature:core/two",
          kind: "feature",
          label: "Core two",
          feature: "core/two",
          state: "planned",
          assumed: false,
        },
        {
          id: "feature:core/three",
          kind: "feature",
          label: "Core three",
          feature: "core/three",
          state: "planned",
          assumed: false,
        },
      ],
      edges: [
        { from: `decision:${ASK_ONE}`, to: "feature:core/two", source_event: ASK_ONE },
        { from: "feature:core/two", to: "feature:core/three", source_event: ulid(191) },
      ],
    });
    render(
      <ClientContext.Provider value={client}>
        <Brief snapshot={client.snapshot} onSelect={vi.fn()} />
      </ClientContext.Provider>,
    );
    expect(
      await screen.findByText("alpha: Core two and 1 more step stays blocked"),
    ).toBeInTheDocument();
    expect(screen.queryByText("alpha: core/two stays blocked")).not.toBeInTheDocument();
  });

  it("keeps an empty section visible with an honest empty line", () => {
    const snapshot = makeSnapshot();
    snapshot.brief.delivered = [];
    const client = new FakeClient(snapshot);
    render(
      <ClientContext.Provider value={client}>
        <Brief snapshot={client.snapshot} onSelect={vi.fn()} />
      </ClientContext.Provider>,
    );

    expect(screen.getByText("DELIVERED")).toBeInTheDocument();
    expect(screen.getByText("No delivery yet")).toBeInTheDocument();
    expect(document.querySelector('[data-brief-empty="delivered"]')).not.toBeNull();
  });

  it("shows All clear only when every one of the six sections is empty", () => {
    const snapshot = makeSnapshot();
    for (const key of Object.keys(snapshot.brief) as (keyof Snapshot["brief"])[]) {
      snapshot.brief[key] = [];
    }
    const client = new FakeClient(snapshot);
    render(
      <ClientContext.Provider value={client}>
        <Brief snapshot={client.snapshot} onSelect={vi.fn()} />
      </ClientContext.Provider>,
    );
    expect(screen.getByRole("heading", { name: "All clear" })).toBeInTheDocument();
  });

  it("opens the row target when clicked", () => {
    const onSelect = vi.fn();
    const client = new FakeClient(makeSnapshot());
    render(
      <ClientContext.Provider value={client}>
        <Brief snapshot={client.snapshot} onSelect={onSelect} />
      </ClientContext.Provider>,
    );
    document.querySelector<HTMLElement>('[data-brief-row="needs"]')?.click();
    expect(onSelect).toHaveBeenCalledWith({ kind: "need", id: "need-one" });
  });

  it("draws the waiting-agent avatars, countdown ring and budget dots from projected data", () => {
    const snapshot = withDelivered(makeDeliverySnapshot());
    // The delivery fixture replaces the alpha project; restore the decision the
    // By-default row points at so its ring is derived, not fabricated.
    snapshot.state.projects.alpha!.decisions[ASK_ONE] = makeDecision(ASK_ONE, easyCard());
    snapshot.state.projects.alpha!.summary.budget = {
      speculative_features: 9,
      pending_slices: 0,
      demo_gap_s: 0,
      blocked: false,
      reasons: [],
    };
    snapshot.brief.at_risk = [
      {
        id: "risk:alpha-budget",
        text: "alpha: 9 of 10 features are built on unconfirmed choices",
        target: { kind: "project", id: "alpha" },
        source_ids: [],
      },
    ];
    const client = new FakeClient(snapshot);
    client.reports.set(textBlob().sha256, {
      mime: "text/plain",
      text: "$ shelf search --author \"Mark Twain\"\n1  Huckleberry Finn",
      sha256: textBlob().sha256,
    });

    render(
      <ClientContext.Provider value={client}>
        <Brief snapshot={client.snapshot} onSelect={vi.fn()} />
      </ClientContext.Provider>,
    );

    expect(screen.getByLabelText("2 agents waiting")).toBeInTheDocument();
    expect(screen.getByTestId("brief-ring").getAttribute("aria-label")).toContain(
      `Default in ${formatRemaining("2026-02-10T18:22:00.000Z", "2026-02-10T15:00:00.000Z")}`,
    );
    expect(
      screen.getByLabelText("9 of 10 speculative features used"),
    ).toBeInTheDocument();
    return waitFor(() =>
      expect(screen.getByTestId("brief-thumbnail")).toBeInTheDocument(),
    );
  });

  it("fills a ten-dot budget meter in proportion to the used features", () => {
    const { container } = render(<BudgetDots used={9} limit={10} />);
    expect(container.querySelectorAll(".dots i")).toHaveLength(10);
    expect(container.querySelectorAll(".dots i:not(.e)")).toHaveLength(9);
  });
});
