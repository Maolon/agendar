/**
 * Pure dependency-graph traversal.
 *
 * Inputs are the immutable `Graph` DTO; nothing here mutates it or reads the
 * DOM. Ancestry uses visited sets so a duplicate edge, a dangling reference or
 * a bad DTO can never recurse forever; staleness of the input is the backend's
 * concern, rendering order is ours.
 */

import { tx } from "../i18n";
import type { Graph, GraphEdge, GraphNode, NodePath, ProjectState } from "../api/types";

export type NodeId = string;

export interface GraphIndex {
  nodes: ReadonlyMap<NodeId, GraphNode>;
  /** Deterministic input order, one entry per node id. */
  order: NodeId[];
  children: ReadonlyMap<NodeId, NodeId[]>;
  parents: ReadonlyMap<NodeId, NodeId[]>;
  /** Edges with both ends known and no self reference, in input order. */
  edges: GraphEdge[];
}

function push(map: Map<NodeId, NodeId[]>, key: NodeId, value: NodeId): void {
  const list = map.get(key);
  if (!list || list.includes(value)) return;
  list.push(value);
}

/**
 * Index nodes and edges once. Dangling references (an edge to a node the DTO
 * did not publish) are dropped from traversal rather than invented.
 */
export function indexGraph(graph: Graph): GraphIndex {
  const nodes = new Map<NodeId, GraphNode>();
  const order: NodeId[] = [];
  for (const node of graph.nodes) {
    if (nodes.has(node.id)) continue;
    nodes.set(node.id, node);
    order.push(node.id);
  }
  const children = new Map<NodeId, NodeId[]>(order.map((id) => [id, []]));
  const parents = new Map<NodeId, NodeId[]>(order.map((id) => [id, []]));
  const edges: GraphEdge[] = [];
  const seen = new Set<string>();
  for (const edge of graph.edges) {
    if (!nodes.has(edge.from) || !nodes.has(edge.to)) continue;
    if (edge.from === edge.to) continue;
    push(children, edge.from, edge.to);
    push(parents, edge.to, edge.from);
    const key = `${edge.from}\u0000${edge.to}`;
    if (seen.has(key)) continue;
    seen.add(key);
    edges.push(edge);
  }
  return { nodes, order, children, parents, edges };
}

function walk(start: NodeId, adjacency: ReadonlyMap<NodeId, NodeId[]>): Set<NodeId> {
  const seen = new Set<NodeId>();
  const queue: NodeId[] = [...(adjacency.get(start) ?? [])];
  while (queue.length) {
    const id = queue.shift();
    if (id === undefined || seen.has(id)) continue;
    seen.add(id);
    for (const next of adjacency.get(id) ?? []) {
      if (!seen.has(next)) queue.push(next);
    }
  }
  return seen;
}

/** Strict ancestors: everything upstream of `id`, never `id` itself. */
export function ancestorsOf(index: GraphIndex, id: NodeId): Set<NodeId> {
  return walk(id, index.parents);
}

/** Strict descendants: everything downstream of `id`, never `id` itself. */
export function descendantsOf(index: GraphIndex, id: NodeId): Set<NodeId> {
  return walk(id, index.children);
}

export interface Highlight {
  /** Selected node plus its ancestors and descendants. */
  nodes: Set<NodeId>;
  /** Edge keys (`from\u0000to`) entirely inside the traced chains. */
  edges: Set<string>;
}

/**
 * Prototype `viewGraph` highlight: an edge stays lit when it connects two
 * traced nodes upstream of the selection, or two traced nodes downstream of it.
 * The selected node itself belongs to both chains, so its own edges light up.
 */
export function highlightSets(index: GraphIndex, selected: NodeId | null): Highlight {
  const nodes = new Set<NodeId>();
  const edges = new Set<NodeId>();
  if (!selected || !index.nodes.has(selected)) return { nodes, edges };
  const upstream = ancestorsOf(index, selected);
  const downstream = descendantsOf(index, selected);
  nodes.add(selected);
  for (const id of upstream) nodes.add(id);
  for (const id of downstream) nodes.add(id);
  for (const edge of index.edges) {
    const inbound =
      (edge.to === selected || upstream.has(edge.to)) && upstream.has(edge.from);
    const outbound =
      (edge.from === selected || downstream.has(edge.from)) && downstream.has(edge.to);
    if (inbound || outbound) edges.add(edgeKey(edge));
  }
  return { nodes, edges };
}

export function edgeKey(edge: { from: NodeId; to: NodeId }): NodeId {
  return `${edge.from}\u0000${edge.to}`;
}

export function isHighlighted(highlight: Highlight, edge: { from: NodeId; to: NodeId }): boolean {
  return highlight.edges.has(edgeKey(edge));
}

/** Display label for a graph node id, preferring the feature's current title. */
export function nodeLabel(
  index: GraphIndex,
  project: ProjectState | null | undefined,
  id: NodeId,
): string {
  const node = index.nodes.get(id);
  const feature: NodePath | undefined = node?.feature;
  if (feature && project?.features[feature]) return project.features[feature].title;
  return node?.label ?? id;
}

/**
 * Descendants in reading order: a supplied layout position first (column then
 * row), otherwise breadth-first distance from the selected node, then the DTO's
 * own node order. Deterministic either way; never lexical-only.
 */
export function orderedDescendants(
  index: GraphIndex,
  id: NodeId,
  position?: (node: NodeId) => { x: number; y: number } | undefined,
): NodeId[] {
  const distance = new Map<NodeId, number>();
  const queue: NodeId[] = [];
  for (const child of index.children.get(id) ?? []) {
    if (distance.has(child)) continue;
    distance.set(child, 1);
    queue.push(child);
  }
  while (queue.length) {
    const current = queue.shift()!;
    for (const child of index.children.get(current) ?? []) {
      if (distance.has(child)) continue;
      distance.set(child, (distance.get(current) ?? 0) + 1);
      queue.push(child);
    }
  }
  const ordinal = new Map<NodeId, number>(index.order.map((node, at) => [node, at]));
  const ids = [...distance.keys()];
  return ids.sort((a, b) => {
    const pa = position?.(a);
    const pb = position?.(b);
    if (pa && pb) {
      if (pa.x !== pb.x) return pa.x - pb.x;
      if (pa.y !== pb.y) return pa.y - pb.y;
    } else if (pa) {
      return -1;
    } else if (pb) {
      return 1;
    }
    const da = distance.get(a) ?? 0;
    const db = distance.get(b) ?? 0;
    if (da !== db) return da - db;
    const oa = ordinal.get(a) ?? 0;
    const ob = ordinal.get(b) ?? 0;
    if (oa !== ob) return oa - ob;
    return a.localeCompare(b);
  });
}

export interface UnblockText {
  labels: string[];
  /** `Unblocks Move to IPC, then Wake idle agents` without markup. */
  text: string;
}

/**
 * The prototype's `unblocksText`: the reachable set, first label first. An
 * empty set is `null` so callers hide the row instead of claiming a release.
 */
export function unblockText(
  index: GraphIndex,
  project: ProjectState | null | undefined,
  id: NodeId,
  position?: (node: NodeId) => { x: number; y: number } | undefined,
): UnblockText | null {
  return textFor(index, project, orderedDescendants(index, id, position));
}

function textFor(
  index: GraphIndex,
  project: ProjectState | null | undefined,
  ids: NodeId[],
): UnblockText | null {
  if (!ids.length) return null;
  const labels = ids.map((node) => nodeLabel(index, project, node));
  const [first, ...rest] = labels;
  const text = rest.length
    ? tx("tr.unblocks_then", { first: first ?? "", rest: rest.join(tx("tr.and")) })
    : tx("tr.unblocks", { first: first ?? "" });
  return { labels, text };
}

/**
 * The unblock set of a review. The real DTO draws `feature → review` and
 * `feature → dependent`, never `review → dependent`, so the reachable set is
 * taken from the delivery's own feature; the review node itself is the thing
 * being accepted and never counts as something it unblocks. A synthetic
 * `review → dependent` edge (dev fixtures) still wins when it exists.
 */
export function reviewUnblockText(
  index: GraphIndex,
  project: ProjectState | null | undefined,
  reviewId: NodeId,
  featureId: NodeId,
  position?: (node: NodeId) => { x: number; y: number } | undefined,
): UnblockText | null {
  const direct = orderedDescendants(index, reviewId, position);
  if (direct.length) return textFor(index, project, direct);
  const throughFeature = orderedDescendants(index, featureId, position).filter(
    (id) => id !== reviewId,
  );
  return textFor(index, project, throughFeature);
}

/**
 * The Brief's “If you wait” row: unique reachable features
 * that stay blocked while this item waits. Returns `null` when the graph holds
 * no node for the item, so the caller keeps the projected row text.
 */
export function ifYouWaitText(
  index: GraphIndex,
  project: ProjectState,
  need: { id: string; node?: NodePath; source: string },
  position?: (node: NodeId) => { x: number; y: number } | undefined,
): string | null {
  const candidates = [
    `decision:${need.source}`,
    `review:${need.source}`,
    need.node ? `feature:${need.node}` : "",
    ...index.order.filter((id) => index.nodes.get(id)?.need_id === need.id),
  ].filter((id) => id && index.nodes.has(id));
  const start = candidates[0];
  if (!start) return null;
  const ids = orderedDescendants(index, start, position);
  if (!ids.length) return null;
  const first = nodeLabel(index, project, ids[0]!);
  const extra = ids.length - 1;
  if (extra === 0) return `${project.summary.slug}: ${first} stays blocked`;
  return `${project.summary.slug}: ${first} and ${extra} more ${
    extra === 1 ? "step stays" : "steps stay"
  } blocked`;
}

/** Node id the backend publishes for a pending decision or review. */
export function decisionNodeId(askId: string): NodeId {
  return `decision:${askId}`;
}

export function reviewNodeId(deliveryId: string): NodeId {
  return `review:${deliveryId}`;
}

export function featureNodeId(node: NodePath): NodeId {
  return `feature:${node}`;
}
