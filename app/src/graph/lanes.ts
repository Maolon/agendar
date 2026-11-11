/**
 * Milestone lanes for the plan map.
 *
 * The full dependency graph stays the source of truth for tracing; lanes only
 * decide where a node is drawn. Each feature sits in the lane of its
 * milestone; a pending decision or review sits in the lane of the feature it
 * gates. Answered decisions and closed reviews are not drawn (they live in the
 * Decisions fold). Edges between lanes are not drawn as long lines: the
 * downstream node carries an "after …" label instead.
 */

import { tx } from "../i18n";
import type { FeatureState, Graph, GraphEdge, GraphNode, NodePath } from "../api/types";
import type { NodeId } from "./traversal";

export const UNSCHEDULED = "";

export interface Lane {
  /** Milestone name; `UNSCHEDULED` for steps without one. */
  key: string;
  label: string;
  graph: Graph;
  features: FeatureState[];
  total: number;
  /** Done (awaiting review) or verified. */
  finished: number;
  /** Doing, stopped mid-way, or waiting on the human. */
  active: number;
  /** Every step is finished: the lane starts collapsed. */
  complete: boolean;
}

export interface LanePlan {
  lanes: Lane[];
  laneOf: ReadonlyMap<NodeId, string>;
  /** Downstream node id -> human label of what it waits on in other lanes. */
  after: ReadonlyMap<NodeId, string>;
}

function isFinished(feature: FeatureState | undefined): boolean {
  return feature?.progress === "done" || feature?.progress === "verified";
}

function isActive(feature: FeatureState): boolean {
  return (
    feature.progress === "doing" ||
    feature.gate === "awaiting_decision" ||
    (feature.liveness === "lost" && !isFinished(feature) && feature.progress !== "dropped")
  );
}

/**
 * Pending decisions and reviews are drawn, and so is an unconfirmed assumption
 * (dashed until a human confirms it). Answered ones are not.
 */
function drawn(node: GraphNode): boolean {
  return node.kind === "feature" || Boolean(node.need_id) || node.assumed;
}

/**
 * The plan as the snapshot alone knows it: every step and its declared
 * dependencies. It stands in for the graph until `get_project` answers, so the
 * lanes, their counts and the room each one takes are right from the first
 * render. Nothing is drawn from it: pending decisions and reviews, and the
 * edges the core rejected, are only in the graph.
 */
export function planOutline(features: Readonly<Record<NodePath, FeatureState>>): Graph {
  const steps = Object.values(features);
  return {
    nodes: steps.map((feature) => ({
      id: `feature:${feature.node}`,
      kind: "feature",
      label: feature.title,
      feature: feature.node,
      state: feature.progress,
      assumed: false,
    })),
    edges: steps.flatMap((feature) =>
      feature.depends_on
        .filter((upstream) => upstream !== feature.node && features[upstream] !== undefined)
        .map((upstream) => ({
          from: `feature:${upstream}`,
          to: `feature:${feature.node}`,
          source_event: feature.field_sources.depends_on ?? "",
        })),
    ),
  };
}

export function buildLanes(graph: Graph, features: Readonly<Record<NodePath, FeatureState>>): LanePlan {
  const nodes = graph.nodes.filter(drawn);
  const ids = new Set(nodes.map((node) => node.id));
  const edges = graph.edges.filter((edge) => ids.has(edge.from) && ids.has(edge.to));
  const laneOf = new Map<NodeId, string>();
  for (const node of nodes) {
    if (node.kind === "feature" && node.feature) {
      laneOf.set(node.id, features[node.feature]?.milestone ?? UNSCHEDULED);
    }
  }
  // A decision or review sits in the lane of the feature it points at.
  for (const node of nodes) {
    if (laneOf.has(node.id)) continue;
    const target = edges.find((edge) => edge.from === node.id && laneOf.has(edge.to));
    laneOf.set(node.id, target ? laneOf.get(target.to)! : UNSCHEDULED);
  }

  const keys = [...new Set(nodes.map((node) => laneOf.get(node.id)!))];
  const order = orderLanes(keys, edges, laneOf, nodes, features);

  const after = new Map<NodeId, string>();
  const featureTitle = (id: NodeId) => nodes.find((node) => node.id === id)?.label ?? id;
  const laneComplete = new Map<string, boolean>();
  for (const key of keys) {
    const inLane = nodes.filter((node) => laneOf.get(node.id) === key && node.feature);
    laneComplete.set(key, inLane.every((node) => isFinished(features[node.feature!])));
  }
  for (const node of nodes) {
    const upstream = edges
      .filter((edge) => edge.to === node.id && laneOf.get(edge.from) !== laneOf.get(node.id))
      .map((edge) => edge.from);
    if (!upstream.length) continue;
    const lanes = [...new Set(upstream.map((id) => laneOf.get(id)!))];
    const whole = lanes.length === 1 && laneComplete.get(lanes[0]!) === true;
    after.set(node.id, whole ? laneLabel(lanes[0]!) : upstream.map(featureTitle).join(", "));
  }

  const lanes = order.map((key): Lane => {
    const laneNodes = nodes.filter((node) => laneOf.get(node.id) === key);
    const laneIds = new Set(laneNodes.map((node) => node.id));
    const laneFeatures = laneNodes
      .map((node) => (node.feature ? features[node.feature] : undefined))
      .filter((feature): feature is FeatureState => feature !== undefined);
    const finished = laneFeatures.filter(isFinished).length;
    return {
      key,
      label: laneLabel(key),
      graph: {
        nodes: laneNodes,
        edges: edges.filter((edge) => laneIds.has(edge.from) && laneIds.has(edge.to)),
      },
      features: laneFeatures,
      total: laneFeatures.length,
      finished,
      active: laneFeatures.filter(isActive).length,
      complete: laneFeatures.length > 0 && finished === laneFeatures.length,
    };
  });
  return { lanes, laneOf, after };
}

export function laneLabel(key: string): string {
  return key === UNSCHEDULED ? tx("gr.other") : key;
}

/**
 * Lanes follow the work: a lane comes after every lane it depends on. Ties
 * put finished lanes first (the past), then lanes in progress, then the rest
 * by name; steps without a milestone always come last.
 */
function orderLanes(
  keys: string[],
  edges: GraphEdge[],
  laneOf: ReadonlyMap<NodeId, string>,
  nodes: GraphNode[],
  features: Readonly<Record<NodePath, FeatureState>>,
): string[] {
  const deps = new Map<string, Set<string>>(keys.map((key) => [key, new Set<string>()]));
  for (const edge of edges) {
    const from = laneOf.get(edge.from);
    const to = laneOf.get(edge.to);
    if (from !== undefined && to !== undefined && from !== to) deps.get(to)!.add(from);
  }
  const stage = (key: string): number => {
    if (key === UNSCHEDULED) return 3;
    const list = nodes
      .filter((node) => laneOf.get(node.id) === key && node.feature)
      .map((node) => features[node.feature!]);
    if (list.length && list.every(isFinished)) return 0;
    if (list.some((feature) => feature && (feature.progress === "doing" || isFinished(feature)))) {
      return 1;
    }
    return 2;
  };
  const rank = (a: string, b: string) => stage(a) - stage(b) || a.localeCompare(b);
  const placed: string[] = [];
  const remaining = new Set(keys);
  while (remaining.size) {
    const ready = [...remaining]
      .filter((key) => [...deps.get(key)!].every((dep) => !remaining.has(dep) || dep === key))
      .sort(rank);
    // A dependency cycle between lanes: fall back to the tie order.
    const next = ready[0] ?? [...remaining].sort(rank)[0]!;
    placed.push(next);
    remaining.delete(next);
  }
  // Unscheduled steps never lead the map.
  return [...placed.filter((key) => key !== UNSCHEDULED), ...placed.filter((key) => key === UNSCHEDULED)];
}

/** Columns shown around the current work in a long lane. */
export const FOCUS_BEFORE = 1;
export const FOCUS_AFTER = 2;
/** A chain this short is always shown whole. */
export const FOCUS_MIN_COLUMNS = 4;

export interface LaneWindow {
  graph: Graph;
  /** Chained steps hidden before / after the window. */
  hiddenBefore: number;
  hiddenAfter: number;
  /** Steps hidden because they are finished or dropped: independent ones, and whole chains with nothing left to do. */
  hiddenFinished: number;
  /** Visible node id -> "after N earlier steps" for hidden upstream. */
  after: ReadonlyMap<NodeId, string>;
}

/**
 * The part of a lane worth looking at: the chain around the work in progress
 * (one column back, two ahead), and the independent steps that are still open.
 * Without work in progress the window sits on the frontier (the last finished
 * column), or on the start when nothing began. Short chains are shown whole;
 * finished and dropped independent steps, and separate chains with nothing
 * left to do, wait behind "Show finished".
 */
export function focusWindow(
  lane: Lane,
  features: Readonly<Record<NodePath, FeatureState>>,
): LaneWindow {
  const doneChains = finishedChains(lane, features);
  const open: Lane = doneChains.size
    ? {
        ...lane,
        graph: {
          nodes: lane.graph.nodes.filter((node) => !doneChains.has(node.id)),
          edges: lane.graph.edges.filter((edge) => !doneChains.has(edge.from) && !doneChains.has(edge.to)),
        },
      }
    : lane;
  const chain = chainWindow(open, features);
  const linked = new Set<NodeId>(open.graph.edges.flatMap((edge) => [edge.from, edge.to]));
  const settled = (node: Graph["nodes"][number]): boolean => {
    if (linked.has(node.id) || !node.feature) return false;
    const feature = features[node.feature];
    return isFinished(feature) || feature?.progress === "dropped";
  };
  const hiddenFinished = chain.graph.nodes.filter(settled).length + doneChains.size;
  if (!hiddenFinished) return chain;
  return {
    ...chain,
    graph: { nodes: chain.graph.nodes.filter((node) => !settled(node)), edges: chain.graph.edges },
    hiddenFinished,
  };
}

/**
 * Nodes of the lane's separate chains whose every step is finished or dropped.
 * Empty when no chain has work left, so a finished lane still shows its last
 * steps.
 */
function finishedChains(lane: Lane, features: Readonly<Record<NodePath, FeatureState>>): Set<NodeId> {
  const root = new Map<NodeId, NodeId>();
  const find = (id: NodeId): NodeId => {
    let at = id;
    while (root.get(at) !== at) at = root.get(at)!;
    return at;
  };
  for (const edge of lane.graph.edges) {
    for (const id of [edge.from, edge.to]) if (!root.has(id)) root.set(id, id);
  }
  for (const edge of lane.graph.edges) root.set(find(edge.from), find(edge.to));
  const chains = new Map<NodeId, NodeId[]>();
  for (const id of root.keys()) chains.set(find(id), [...(chains.get(find(id)) ?? []), id]);
  const byId = new Map(lane.graph.nodes.map((node) => [node.id, node]));
  const settled = (id: NodeId) => {
    const feature = byId.get(id)?.feature ? features[byId.get(id)!.feature!] : undefined;
    return feature !== undefined && (isFinished(feature) || feature.progress === "dropped");
  };
  const done = [...chains.values()].filter((ids) => ids.every(settled));
  return done.length === chains.size ? new Set() : new Set(done.flat());
}

function chainWindow(
  lane: Lane,
  features: Readonly<Record<NodePath, FeatureState>>,
): LaneWindow {
  const whole: LaneWindow = {
    graph: lane.graph,
    hiddenBefore: 0,
    hiddenAfter: 0,
    hiddenFinished: 0,
    after: new Map(),
  };
  const edges = lane.graph.edges;
  const linked = new Set<NodeId>(edges.flatMap((edge) => [edge.from, edge.to]));
  if (!linked.size) return whole;
  const parents = new Map<NodeId, NodeId[]>();
  for (const edge of edges) parents.set(edge.to, [...(parents.get(edge.to) ?? []), edge.from]);
  const depth = new Map<NodeId, number>();
  const visiting = new Set<NodeId>();
  const depthOf = (id: NodeId): number => {
    const known = depth.get(id);
    if (known !== undefined) return known;
    if (visiting.has(id)) return 0;
    visiting.add(id);
    const value = Math.max(0, ...(parents.get(id) ?? []).map((parent) => depthOf(parent) + 1));
    visiting.delete(id);
    depth.set(id, value);
    return value;
  };
  for (const id of linked) depthOf(id);
  const maxDepth = Math.max(...depth.values());
  if (maxDepth + 1 <= FOCUS_MIN_COLUMNS) return whole;

  const featureOf = (id: NodeId) => {
    const node = lane.graph.nodes.find((item) => item.id === id);
    return node?.feature ? features[node.feature] : undefined;
  };
  const activeDepths = [...linked]
    .filter((id) => {
      const node = lane.graph.nodes.find((item) => item.id === id);
      if (node && node.kind !== "feature") return true;
      const feature = featureOf(id);
      return feature !== undefined && (isActive(feature) || feature.gate === "awaiting_decision");
    })
    .map((id) => depth.get(id)!);
  let focusLo: number;
  let focusHi: number;
  if (activeDepths.length) {
    focusLo = Math.min(...activeDepths);
    focusHi = Math.max(...activeDepths);
  } else {
    const finishedDepths = [...linked].filter((id) => isFinished(featureOf(id))).map((id) => depth.get(id)!);
    focusLo = focusHi = finishedDepths.length ? Math.max(...finishedDepths) : 0;
  }
  let lo = Math.max(0, focusLo - FOCUS_BEFORE);
  let hi = Math.min(maxDepth, focusHi + FOCUS_AFTER);
  // Keep at least three columns on screen when the chain allows it.
  while (hi - lo < 2 && (lo > 0 || hi < maxDepth)) {
    if (lo > 0) lo -= 1;
    else hi += 1;
  }
  const hidden = (id: NodeId) => linked.has(id) && (depth.get(id)! < lo || depth.get(id)! > hi);
  const nodes = lane.graph.nodes.filter((node) => !hidden(node.id));
  const ids = new Set(nodes.map((node) => node.id));
  const hiddenBefore = [...linked].filter((id) => depth.get(id)! < lo).length;
  const hiddenAfter = [...linked].filter((id) => depth.get(id)! > hi).length;
  const after = new Map<NodeId, string>();
  for (const node of nodes) {
    if ((parents.get(node.id) ?? []).some((parent) => !ids.has(parent))) {
      after.set(node.id, `${hiddenBefore} earlier step${hiddenBefore === 1 ? "" : "s"}`);
    }
  }
  return {
    graph: { nodes, edges: edges.filter((edge) => ids.has(edge.from) && ids.has(edge.to)) },
    hiddenBefore,
    hiddenAfter,
    hiddenFinished: 0,
    after,
  };
}
