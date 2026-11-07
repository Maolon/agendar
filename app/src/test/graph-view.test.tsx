import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { ClientContext, SnapshotStore } from "../api/store";
import { createClient } from "../api/client";
import type { Graph } from "../api/types";
import { createFixtureTransport } from "../dev/fixtureTransport";
import { App } from "../App";
import { Graph as GraphView } from "../components/Graph";
import { Project } from "../views/Project";
import { FakeClient, makeGraph, projectProps } from "./fixtures";
import { ASK, planGraph, planProject } from "./graph-fixtures";

function renderGraph(selected: string | null = null, onOpenNeed = vi.fn(), onSelect = vi.fn()) {
  const project = planProject();
  const view = render(
    <GraphView
      graph={planGraph()}
      project={project}
      selected={selected}
      onSelect={onSelect}
      onOpenNeed={onOpenNeed}
      motion={false}
      pulses={[]}
      pulseToken={0}
      play={null}
      onPlayed={() => undefined}
    />,
  );
  return { project, onOpenNeed, onSelect, ...view };
}

describe("dependency graph", () => {
  it("shows state, basis and ETA from the feature records", () => {
    renderGraph();
    const hotkeys = document.querySelector<HTMLElement>('[data-graph-node="feature:core/hotkeys"]');
    expect(hotkeys?.className).toContain("assumed");
    const async = document.querySelector<HTMLElement>('[data-graph-node="feature:core/async"]');
    expect(async?.className).toContain("tainted");
    // Basis and ETA both come from the feature record, never the DTO.
    expect(within(async as HTMLElement).getByText("Rework · 15m late")).toBeInTheDocument();
    const menubar = document.querySelector<HTMLElement>('[data-graph-node="feature:core/menubar"]');
    expect(within(menubar as HTMLElement).getByText("Menu bar triage")).toBeInTheDocument();
    const decision = document.querySelector<HTMLElement>(`[data-graph-node="decision:${ASK}"]`);
    expect(decision?.className).toContain("you");
    expect(within(decision as HTMLElement).getByText("Your call")).toBeInTheDocument();
  });

  it("opens a pending amber node through its explicit action and on double click", async () => {
    const { onOpenNeed, onSelect } = renderGraph();
    await userEvent.click(
      screen.getByRole("button", { name: "Open How should answers reach idle agents?" }),
    );
    expect(onOpenNeed).toHaveBeenCalledWith("need-ipc");
    // The single click still traces instead of leaving the graph.
    expect(onSelect).not.toHaveBeenCalled();

    onOpenNeed.mockClear();
    await userEvent.dblClick(
      screen.getByRole("button", {
        name: /How should answers reach idle agents\?.*Trace what it waits on/,
      }),
    );
    expect(onOpenNeed).toHaveBeenCalledWith("need-ipc");
  });

  it("dims everything outside the traced chains and highlights only their edges", () => {
    renderGraph("feature:core/ipc");
    const classOf = (id: string) =>
      document.querySelector<HTMLElement>(`[data-graph-node="${id}"]`)?.className ?? "";
    expect(classOf("feature:core/ask")).not.toContain("dim");
    expect(classOf(`decision:${ASK}`)).not.toContain("dim");
    expect(classOf("feature:core/wake")).not.toContain("dim");
    expect(classOf("feature:core/menubar")).toContain("dim");
    expect(classOf("feature:core/hotkeys")).toContain("dim");

    const edge = (from: string, to: string) =>
      document.querySelector(`path[data-from="${from}"][data-to="${to}"]`)?.getAttribute("class");
    expect(edge("feature:core/ask", "feature:core/ipc")).toBe("ge hl");
    expect(edge(`decision:${ASK}`, "feature:core/ipc")).toBe("ge hl");
    expect(edge("feature:core/ipc", "feature:core/wake")).toBe("ge hl");
    expect(edge("feature:core/ask", "feature:core/menubar")).toBe("ge dim");
  });

  it("draws the dev fixture plan and clears the trace on Escape first", async () => {
    const client = createClient(createFixtureTransport(), "fixture");
    const store = new SnapshotStore(client);
    render(<App client={client} store={store} surface="main" />);
    await screen.findByRole("heading", { name: /Since you last looked/ });
    await userEvent.click(
      document.querySelector<HTMLElement>('[data-row="project:agendar"]') as HTMLElement,
    );
    // Five features plus the pending call, straight from the dev fixture graph.
    await waitFor(() =>
      expect(document.querySelectorAll("[data-graph-node]")).toHaveLength(6),
    );
    await userEvent.click(
      screen.getByRole("button", { name: /CLI ask\. .*Trace what it waits on/ }),
    );
    expect(
      document.querySelector('[data-graph-node="feature:core/ask"] [aria-pressed="true"]'),
    ).not.toBeNull();

    await userEvent.keyboard("{Escape}");
    // The trace clears before Escape means “back to the brief”.
    expect(
      document.querySelector('[data-graph-node="feature:core/ask"] [aria-pressed="true"]'),
    ).toBeNull();
    expect(screen.getByRole("heading", { name: "Agendar" })).toBeInTheDocument();
  });

  it("renders the dev fixture's defaulted assumption as static and opens the real pending call", async () => {
    const client = createClient(createFixtureTransport(), "fixture");
    const store = new SnapshotStore(client);
    render(<App client={client} store={store} surface="main" />);
    await screen.findByRole("heading", { name: /Since you last looked/ });

    // screen-clips's only decision is a defaulted assumption: dashed, not amber,
    // and it must not offer an Open control it cannot honour.
    await userEvent.click(
      document.querySelector<HTMLElement>('[data-row="project:screen-clips"]') as HTMLElement,
    );
    await waitFor(() => expect(document.querySelectorAll("[data-graph-node]")).toHaveLength(4));
    const assumed = document.querySelector<HTMLElement>('[data-graph-node^="decision:"]');
    expect(assumed?.className).toContain("assumed");
    expect(assumed?.className).not.toContain("you");
    expect(assumed?.textContent).toContain("Assumed · not confirmed");
    expect(within(assumed as HTMLElement).queryByRole("button", { name: /^Open / })).toBeNull();

    // The real pending amber card lives on agendar, with its exact need.
    await userEvent.click(
      document.querySelector<HTMLElement>('[data-row="project:agendar"]') as HTMLElement,
    );
    await userEvent.click(
      await screen.findByRole("button", { name: "Open How should answers reach idle agents?" }),
    );
    await screen.findByRole("heading", { name: "How should answers reach idle agents?" });
  });

  it("never scales a wide plan down; it scrolls instead so titles stay readable", () => {
    const chain: Graph = makeGraph({
      nodes: ["feature:a", "feature:b", "feature:c", "feature:d"].map((id, index) => ({
        id,
        kind: "feature" as const,
        label: `Step ${index + 1}`,
        feature: `step/${index + 1}`,
        state: "planned",
        assumed: false,
      })),
      edges: [
        { from: "feature:a", to: "feature:b", source_event: ASK },
        { from: "feature:b", to: "feature:c", source_event: ASK },
        { from: "feature:c", to: "feature:d", source_event: ASK },
      ],
    });
    render(
      <GraphView
        graph={chain}
        project={planProject()}
        selected={null}
        onSelect={vi.fn()}
        onOpenNeed={vi.fn()}
        motion={false}
        pulses={[]}
        pulseToken={0}
        play={null}
        onPlayed={() => undefined}
      />,
    );
    // Four columns are 724 px wide at full size; the stage keeps that size and
    // the .gwrap container scrolls horizontally (overflow-x: auto).
    const stage = document.querySelector<HTMLElement>(".gstage");
    expect(stage?.style.width).toBe("724px");
    const graph = document.querySelector<HTMLElement>(".graph");
    expect(graph?.style.transform ?? "").toBe("");
  });

  it("draws no edges and says so when the plan is empty", () => {
    render(
      <GraphView
        graph={makeGraph()}
        project={planProject()}
        selected={null}
        onSelect={vi.fn()}
        onOpenNeed={vi.fn()}
        motion={false}
        pulses={[]}
        pulseToken={0}
        play={null}
        onPlayed={() => undefined}
      />,
    );
    expect(screen.getByText("No steps planned.")).toBeInTheDocument();
    expect(document.querySelectorAll("[data-graph-node]")).toHaveLength(0);
  });

  it("selects the same node from an In-flight row and clears the trace again", async () => {
    const client = new FakeClient();
    client.graph = planGraph();
    const project = planProject();
    function Harness() {
      const [node, setNode] = useState<string | null>(null);
      return (
        <ClientContext.Provider value={client}>
          <Project
            {...projectProps(project, client, {
              selectedNode: node ?? undefined,
              onSelectNode: setNode,
            })}
          />
        </ClientContext.Provider>
      );
    }
    render(<Harness />);

    const row = await screen.findByRole("button", { name: /Menu bar triage/ });
    await userEvent.click(row);
    const node = document.querySelector<HTMLElement>('[data-graph-node="feature:core/menubar"]');
    expect(node?.querySelector('[aria-pressed="true"]')).not.toBeNull();
    // Ancestors stay lit; an unrelated branch is dimmed.
    expect(document.querySelector('[data-graph-node="feature:core/ask"]')?.className ?? "").not.toContain(
      "dim",
    );
    expect(document.querySelector('[data-graph-node="feature:core/wake"]')?.className).toContain(
      "dim",
    );

    await userEvent.click(screen.getByRole("button", { name: "Show all" }));
    expect(document.querySelector('[data-graph-node="feature:core/wake"]')?.className ?? "").not.toContain(
      "dim",
    );
    expect(node?.querySelector('[aria-pressed="true"]')).toBeNull();
  });
});
