import { describe, expect, it } from "vitest";
import type { Graph, GraphNode } from "../api/types";
import { layoutGraph, COLW, NODEW, ROWH } from "../graph/layout";
import {
  ancestorsOf,
  descendantsOf,
  highlightSets,
  indexGraph,
  ifYouWaitText,
  isHighlighted,
  orderedDescendants,
  reviewNodeId,
  decisionNodeId,
  featureNodeId,
  unblockText,
} from "../graph/traversal";
import { makeProject, sha } from "./fixtures";

function feature(id: string, state: string, label = id): GraphNode {
  return { id: `feature:${id}`, kind: "feature", label, feature: id, state, assumed: false };
}

function node(id: string, kind: "decision" | "review", label: string): GraphNode {
  return { id, kind, label, state: "pending", assumed: false };
}

function edge(from: string, to: string) {
  return { from, to, source_event: sha(0x70) };
}

/** The prototype's `agendar` plan: call node inserted between ask and wake. */
function agendarGraph(): Graph {
  return {
    nodes: [
      feature("ask", "verified", "CLI ask"),
      node(decisionNodeId("ask-ulid"), "decision", "Wake method?"),
      feature("wake", "planned", "Wake idle agents"),
      feature("menubar", "doing", "Menu bar triage"),
      feature("async", "planned", "Async wait"),
      feature("demo", "planned", "Slice 1 demo"),
    ],
    edges: [
      edge(featureNodeId("ask"), featureNodeId("wake")),
      edge(decisionNodeId("ask-ulid"), featureNodeId("wake")),
      edge(featureNodeId("ask"), featureNodeId("menubar")),
      edge(featureNodeId("wake"), featureNodeId("async")),
      edge(featureNodeId("menubar"), featureNodeId("demo")),
      edge(featureNodeId("async"), featureNodeId("demo")),
    ],
  };
}

/** The prototype's `screen-clips` plan: diamond through a decision node. */
function apBrowserGraph(): Graph {
  return {
    nodes: [
      feature("whole", "verified", "Whole screen"),
      feature("region", "doing", "Region"),
      feature("encoder", "doing", "Encoder"),
      node(decisionNodeId("webm"), "decision", "Keep WebM?"),
      feature("export", "planned", "Export & share"),
      feature("hotkeys", "planned", "Hotkeys"),
      feature("release", "planned", "Release v2"),
    ],
    edges: [
      edge(featureNodeId("whole"), featureNodeId("region")),
      edge(featureNodeId("whole"), featureNodeId("encoder")),
      edge(featureNodeId("region"), featureNodeId("export")),
      edge(featureNodeId("encoder"), featureNodeId("export")),
      edge(decisionNodeId("webm"), featureNodeId("export")),
      edge(featureNodeId("region"), featureNodeId("hotkeys")),
      edge(featureNodeId("export"), featureNodeId("release")),
      edge(featureNodeId("hotkeys"), featureNodeId("release")),
    ],
  };
}

describe("graph layout", () => {
  it("returns zero size and no nodes for an empty graph", () => {
    const layout = layoutGraph({ nodes: [], edges: [] });
    expect(layout.width).toBe(0);
    expect(layout.height).toBe(0);
    expect(layout.nodes).toEqual([]);
    expect(layout.diagnostic).toBeNull();
  });

  it("matches the prototype agendar geometry", () => {
    const layout = layoutGraph(agendarGraph());
    const at = (id: string) => layout.positions.get(id);
    expect(at(featureNodeId("ask"))).toMatchObject({ x: 0, y: 0, depth: 0 });
    expect(at(decisionNodeId("ask-ulid"))).toMatchObject({ x: 0, y: 60, depth: 0 });
    // Menubar keeps row 0 in column 1 because its parent is ask at row 0; the
    // decision's mean (+0.5) puts wake below it.
    expect(at(featureNodeId("menubar"))).toMatchObject({ x: 188, y: 0, depth: 1 });
    expect(at(featureNodeId("wake"))).toMatchObject({ x: 188, y: 60, depth: 1 });
    expect(at(featureNodeId("async"))).toMatchObject({ x: 376, y: 0, depth: 2 });
    expect(at(featureNodeId("demo"))).toMatchObject({ x: 564, y: 0, depth: 3 });
    expect(layout.width).toBe(3 * COLW + NODEW);
    expect(layout.height).toBe(ROWH + 46);
  });

  it("matches the prototype screen-clips diamond geometry", () => {
    const layout = layoutGraph(apBrowserGraph());
    expect(layout.positions.get(featureNodeId("region"))).toMatchObject({ x: 188, y: 0 });
    expect(layout.positions.get(featureNodeId("encoder"))).toMatchObject({ x: 188, y: 60 });
    expect(layout.positions.get(decisionNodeId("webm"))).toMatchObject({ x: 188, y: 120 });
    expect(layout.positions.get(featureNodeId("hotkeys"))).toMatchObject({ x: 376, y: 0 });
    expect(layout.positions.get(featureNodeId("export"))).toMatchObject({ x: 376, y: 60 });
    expect(layout.positions.get(featureNodeId("release"))).toMatchObject({ x: 564, y: 0 });
    expect(layout.diagnostic).toBeNull();
  });

  it("keeps the four-column plan at the documented 724 px geometry", () => {
    const layout = layoutGraph(agendarGraph());
    // Four columns; the view scales this plan into the real 715 px reading
    // column (checked in graph-view.test.tsx), so the geometry stays exact.
    expect(layout.width).toBe(724);
  });

  it("places roots left of children and never overlaps two nodes", () => {
    const layout = layoutGraph(apBrowserGraph());
    for (const { id, position } of layout.nodes) {
      for (const other of layout.nodes) {
        if (other.id === id) continue;
        const clash =
          other.position.x < position.x + NODEW &&
          position.x < other.position.x + NODEW &&
          other.position.y < position.y + 46 &&
          position.y < other.position.y + 46;
        expect(clash, `${id} overlaps ${other.id}`).toBe(false);
      }
    }
    for (const edge of layout.edges) {
      const from = layout.positions.get(edge.from)!;
      const to = layout.positions.get(edge.to)!;
      expect(from.depth).toBeLessThan(to.depth);
    }
  });

  it("keeps a decision chain from drawing backwards", () => {
    const graph: Graph = {
      nodes: [
        node(decisionNodeId("a"), "decision", "First"),
        node(decisionNodeId("b"), "decision", "Second"),
        feature("one", "planned"),
        feature("two", "planned"),
      ],
      edges: [
        edge(decisionNodeId("a"), decisionNodeId("b")),
        edge(decisionNodeId("b"), featureNodeId("one")),
        edge(featureNodeId("one"), featureNodeId("two")),
      ],
    };
    const layout = layoutGraph(graph);
    for (const edge of layout.edges) {
      expect(layout.positions.get(edge.from)!.depth).toBeLessThan(
        layout.positions.get(edge.to)!.depth,
      );
    }
  });

  it("reports a cycle instead of recursing or claiming a sound order", () => {
    const graph: Graph = {
      nodes: [feature("a", "planned"), feature("b", "planned"), feature("c", "planned")],
      edges: [
        edge(featureNodeId("a"), featureNodeId("b")),
        edge(featureNodeId("b"), featureNodeId("c")),
        edge(featureNodeId("c"), featureNodeId("a")),
      ],
    };
    const layout = layoutGraph(graph);
    expect(layout.diagnostic).not.toBeNull();
    expect(layout.nodes).toHaveLength(3);
  });

  it("sorts a single node, disconnected roots and a review insertion", () => {
    const single = layoutGraph({ nodes: [feature("only", "verified")], edges: [] });
    expect(single.width).toBe(NODEW);
    expect(single.height).toBe(46);

    const roots: Graph = {
      nodes: [feature("a", "planned"), feature("b", "planned"), feature("c", "planned")],
      edges: [],
    };
    const rootLayout = layoutGraph(roots);
    expect(rootLayout.positions.get(featureNodeId("b"))).toMatchObject({ x: 0, y: 60 });

    const withReview: Graph = {
      nodes: [
        feature("done", "done"),
        node(reviewNodeId("delivery"), "review", "Review delivery"),
        feature("next", "planned"),
      ],
      edges: [
        edge(featureNodeId("done"), reviewNodeId("delivery")),
        edge(reviewNodeId("delivery"), featureNodeId("next")),
      ],
    };
    const reviewLayout = layoutGraph(withReview);
    expect(reviewLayout.positions.get(reviewNodeId("delivery"))!.depth).toBe(1);
    expect(reviewLayout.positions.get(featureNodeId("next"))!.depth).toBe(2);
  });

  it("draws edges through the gap's bus with an arrow-sized gap before the child", () => {
    const layout = layoutGraph(agendarGraph());
    const path = layout.edges.find(
      (edge) => edge.from === featureNodeId("ask") && edge.to === featureNodeId("wake"),
    );
    expect(path?.path).toBe(`M160 23 H174 V83 H185`);
  });

  const busX = (path: string) => Number(/H(-?[\d.]+) V/.exec(path)?.[1]);

  it("routes a fan of dependencies through one bus per gap", () => {
    const sources = ["a", "b"];
    const targets = ["t1", "t2", "t3", "t4"];
    const layout = layoutGraph({
      nodes: [...sources, ...targets].map((id) => feature(id, "planned")),
      edges: sources.flatMap((s) => targets.map((t) => edge(featureNodeId(s), featureNodeId(t)))),
    });
    const xs = new Set(layout.edges.map((e) => busX(e.path)));
    expect(xs.size).toBe(1);
    expect(layout.markers).toEqual([]);
  });

  it("gives unconnected groups in one gap their own bus", () => {
    // a feeds rows 0–1, b feeds row 2 from row 1: the groups' spans overlap.
    const layout = layoutGraph({
      nodes: ["a", "b", "t1", "t3", "t2"].map((id) => feature(id, "planned")),
      edges: [
        edge(featureNodeId("a"), featureNodeId("t1")),
        edge(featureNodeId("a"), featureNodeId("t3")),
        edge(featureNodeId("b"), featureNodeId("t2")),
      ],
    });
    const of = (from: string) =>
      new Set(layout.edges.filter((e) => e.from === featureNodeId(from)).map((e) => busX(e.path)));
    expect(of("a").size).toBe(1);
    expect(of("b").size).toBe(1);
    expect([...of("a")][0]).not.toBe([...of("b")][0]);
  });

  it("drops an edge implied by a longer path and pairs a skipping edge with markers", () => {
    const layout = layoutGraph({
      nodes: ["a", "b", "c", "z"].map((id) => feature(id, "planned")),
      edges: [
        edge(featureNodeId("a"), featureNodeId("b")),
        edge(featureNodeId("b"), featureNodeId("c")),
        edge(featureNodeId("a"), featureNodeId("c")),
        // z skips a column to reach c with no path through b.
        edge(featureNodeId("z"), featureNodeId("c")),
      ],
    });
    const drawn = layout.edges.filter((e) => !e.marker).map((e) => `${e.from}>${e.to}`);
    expect(drawn).not.toContain(`${featureNodeId("a")}>${featureNodeId("c")}`);
    const marked = layout.edges.filter((e) => e.marker).map((e) => `${e.from}>${e.to}`);
    expect(marked).toEqual([`${featureNodeId("z")}>${featureNodeId("c")}`]);
    expect(layout.markers.map((m) => `${m.kind}:${m.label}`).sort()).toEqual(["in:1", "out:1"]);
  });
});

describe("graph traversal", () => {
  it("finds ancestors and descendants through a decision node", () => {
    const index = indexGraph(agendarGraph());
    expect([...ancestorsOf(index, featureNodeId("async"))].sort()).toEqual([
      decisionNodeId("ask-ulid"),
      featureNodeId("ask"),
      featureNodeId("wake"),
    ]);
    expect([...descendantsOf(index, decisionNodeId("ask-ulid"))].sort()).toEqual([
      featureNodeId("async"),
      featureNodeId("demo"),
      featureNodeId("wake"),
    ]);
  });

  it("highlights only edges inside the traced chains", () => {
    const graph = apBrowserGraph();
    const index = indexGraph(graph);
    const highlight = highlightSets(index, featureNodeId("export"));
    // Ancestors: whole, region, encoder, webm. Descendants: release.
    expect(highlight.nodes.has(featureNodeId("hotkeys"))).toBe(false);
    expect(highlight.nodes.has(featureNodeId("release"))).toBe(true);
    expect(
      isHighlighted(highlight, { from: featureNodeId("region"), to: featureNodeId("export") }),
    ).toBe(true);
    expect(
      isHighlighted(highlight, { from: decisionNodeId("webm"), to: featureNodeId("export") }),
    ).toBe(true);
    expect(
      isHighlighted(highlight, { from: featureNodeId("export"), to: featureNodeId("release") }),
    ).toBe(true);
    expect(
      isHighlighted(highlight, { from: featureNodeId("region"), to: featureNodeId("hotkeys") }),
    ).toBe(false);
    expect(
      isHighlighted(highlight, { from: featureNodeId("hotkeys"), to: featureNodeId("release") }),
    ).toBe(false);
  });

  it("clears every highlight set without a selection", () => {
    const index = indexGraph(apBrowserGraph());
    const highlight = highlightSets(index, null);
    expect(highlight.nodes.size).toBe(0);
    expect(highlight.edges.size).toBe(0);
  });

  it("orders descendants by column then row", () => {
    const graph = apBrowserGraph();
    const index = indexGraph(graph);
    const layout = layoutGraph(graph);
    const ids = orderedDescendants(index, featureNodeId("whole"), (id) =>
      layout.positions.get(id),
    );
    expect(ids).toEqual([
      featureNodeId("region"),
      featureNodeId("encoder"),
      featureNodeId("hotkeys"),
      featureNodeId("export"),
      featureNodeId("release"),
    ]);
  });

  it("reads the unblock text from the reachable set", () => {
    const graph = agendarGraph();
    const index = indexGraph(graph);
    const layout = layoutGraph(graph);
    const project = makeProject("agendar");
    project.features["wake"] = { ...project.features["core/one"]!, node: "wake", title: "Wake idle agents" };
    const text = unblockText(index, project, decisionNodeId("ask-ulid"), (id) =>
      layout.positions.get(id),
    );
    expect(text?.text).toBe(
      "Unblocks Wake idle agents, then Async wait and Slice 1 demo",
    );
    expect(unblockText(index, project, featureNodeId("demo"))).toBeNull();
  });

  it("writes the If-you-wait sentence from the reachable blocked steps", () => {
    const graph = agendarGraph();
    const index = indexGraph(graph);
    const layout = layoutGraph(graph);
    const project = makeProject("agendar");
    project.features["wake"] = { ...project.features["core/one"]!, node: "wake", title: "Wake idle agents" };
    const position = (id: string) => layout.positions.get(id);
    expect(
      ifYouWaitText(
        index,
        project,
        { id: "need-1", source: "ask-ulid", node: "wake" },
        position,
      ),
    ).toBe("agendar: Wake idle agents and 2 more steps stay blocked");
    expect(
      ifYouWaitText(index, project, { id: "missing", source: "nothing" }, position),
    ).toBeNull();
  });

  it("ignores dangling, self and duplicate edges", () => {
    const graph: Graph = {
      nodes: [feature("a", "planned"), feature("b", "planned")],
      edges: [
        edge(featureNodeId("a"), featureNodeId("b")),
        edge(featureNodeId("a"), featureNodeId("b")),
        edge(featureNodeId("a"), featureNodeId("a")),
        edge(featureNodeId("a"), "feature:gone"),
      ],
    };
    const index = indexGraph(graph);
    expect(index.edges).toHaveLength(1);
    expect(index.children.get(featureNodeId("a"))).toEqual([featureNodeId("b")]);
    const layout = layoutGraph(graph);
    expect(layout.edges).toHaveLength(1);
  });
});
