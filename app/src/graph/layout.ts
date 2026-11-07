/**
 * Dependency-graph geometry: the prototype's layered
 * left-to-right layout, ported without mutating the DTO.
 *
 * Deterministic inputs (the DTO's node order) produce deterministic positions.
 * The layout is pure so tests can assert exact geometry without a DOM, and the
 * SVG paths are returned alongside the positions so a packet animation can
 * follow the same curve.
 */

import { tx } from "../i18n";
import type { Graph } from "../api/types";
import { indexGraph, type GraphIndex, type NodeId } from "./traversal";

export const COLW = 188;
export const NODEW = 160;
export const NODEH = 46;
export const ROWH = 60;

/** Plan-map lanes: wider nodes with room for a two-line title, never scaled. */
export const LANE_NODEW = 236;
export const LANE_NODEH = 62;
export const LANE_GAP_X = 44;
export const LANE_GAP_Y = 14;
export const FREE_GAP_X = 14;
/** Room above the independent-steps grid for its caption. */
export const FREE_CAPTION = 26;

export interface GraphPoint {
  x: number;
  y: number;
  /** Column index; 0 is the leftmost root column. */
  depth: number;
  /** Row index inside the column. */
  row: number;
}

export interface PlacedNode {
  id: NodeId;
  position: GraphPoint;
}

export interface PlacedEdge {
  from: NodeId;
  to: NodeId;
  path: string;
  /** Drawn as a numbered marker pair instead of a line (wrapped or skipping columns). */
  marker?: boolean;
}

/**
 * A numbered badge on a card edge: `out` on a step whose edge continues
 * elsewhere (next band, or past a column), `in` on the step it reaches. The
 * same number pairs them, like a flowchart's off-page connector.
 */
export interface GraphMarker {
  kind: "out" | "in";
  node: NodeId;
  label: string;
  /** Badge centre and width. */
  x: number;
  y: number;
  width: number;
  edges: { from: NodeId; to: NodeId }[];
}

interface Link {
  from: NodeId;
  to: NodeId;
}

export interface GraphLayout {
  width: number;
  height: number;
  positions: ReadonlyMap<NodeId, GraphPoint>;
  nodes: PlacedNode[];
  edges: PlacedEdge[];
  /** Numbered badges for edges drawn as marker pairs. */
  markers: GraphMarker[];
  /** The edges worth drawing: the DTO's, less those implied by a longer path. */
  links?: Link[];
  /**
   * Set when the DTO contains a cycle or a backwards edge that the safety
   * extension could not repair: display the graph, never claim it is sound.
   */
  diagnostic: string | null;
  /** Node box size; the classic layout uses NODEW x NODEH. */
  nodeWidth?: number;
  nodeHeight?: number;
  /** Top of the independent-steps grid caption, when a lane has one. */
  freeTop?: number | null;
}

interface Dims {
  colW: number;
  rowH: number;
  nodeW: number;
  nodeH: number;
}

const CLASSIC: Dims = { colW: COLW, rowH: ROWH, nodeW: NODEW, nodeH: NODEH };

export function emptyLayout(): GraphLayout {
  return {
    width: 0,
    height: 0,
    positions: new Map(),
    nodes: [],
    edges: [],
    markers: [],
    diagnostic: null,
  };
}

function isDecision(index: GraphIndex, id: NodeId): boolean {
  return index.nodes.get(id)?.kind === "decision";
}

/**
 * Depth of one non-decision node: `1 + max(depth of non-decision parents)`,
 * roots at 0 (prototype `d()` ignores decision predecessors). The visiting set
 * returns a diagnostic instead of overflowing on a bad DTO.
 */
function depthsFor(index: GraphIndex): { depths: Map<NodeId, number>; cyclic: boolean } {
  const depths = new Map<NodeId, number>();
  const visiting = new Set<NodeId>();
  let cyclic = false;

  const visit = (id: NodeId): number => {
    const known = depths.get(id);
    if (known !== undefined) return known;
    if (visiting.has(id)) {
      cyclic = true;
      return 0;
    }
    visiting.add(id);
    let highest = -1;
    for (const parent of index.parents.get(id) ?? []) {
      if (isDecision(index, parent)) continue;
      highest = Math.max(highest, visit(parent));
    }
    visiting.delete(id);
    const depth = highest + 1;
    depths.set(id, depth);
    return depth;
  };

  for (const id of index.order) {
    if (!isDecision(index, id)) visit(id);
  }
  return { depths, cyclic };
}

/**
 * Decision nodes sit one column before their earliest child; then a bounded
 * topological pass pushes every child right of every parent so a decision chain
 * or a diamond can never draw backwards (spec step 3).
 */
function placeDepths(
  index: GraphIndex,
  depths: Map<NodeId, number>,
  cyclic: boolean,
): { depths: Map<NodeId, number>; cyclic: boolean } {
  for (const id of index.order) {
    if (!isDecision(index, id)) continue;
    const children = index.children.get(id) ?? [];
    let earliest = children.length ? Number.POSITIVE_INFINITY : 1;
    for (const child of children) {
      const depth = depths.get(child);
      if (depth !== undefined) earliest = Math.min(earliest, depth);
    }
    if (!Number.isFinite(earliest)) earliest = 1;
    depths.set(id, Math.max(0, earliest - 1));
  }

  let changed = true;
  let passes = 0;
  while (changed && passes <= index.order.length) {
    changed = false;
    passes += 1;
    for (const edge of index.edges) {
      const parent = depths.get(edge.from);
      const child = depths.get(edge.to);
      if (parent === undefined || child === undefined) continue;
      if (child < parent + 1) {
        depths.set(edge.to, parent + 1);
        changed = true;
      }
    }
  }
  if (changed) cyclic = true;
  return { depths, cyclic };
}

function stableSort<T>(items: T[], key: (item: T) => number, tie: (a: T, b: T) => number): T[] {
  return items
    .map((item, ordinal) => ({ item, ordinal }))
    .sort((a, b) => {
      const primary = key(a.item) - key(b.item);
      if (Number.isNaN(primary)) return a.ordinal - b.ordinal;
      if (primary !== 0) return primary;
      const secondary = tie(a.item, b.item);
      if (secondary !== 0) return secondary;
      return a.ordinal - b.ordinal;
    })
    .map((entry) => entry.item);
}

/**
 * Lay out a `Graph` DTO. Empty input returns width/height 0, matching the
 * “No steps planned” state.
 */
export function layoutGraph(graph: Graph): GraphLayout {
  const index = indexGraph(graph);
  if (!index.order.length) return emptyLayout();
  return layered(index, CLASSIC);
}

/** Layered left-to-right placement shared by the classic graph and the lanes. */
function layered(index: GraphIndex, dims: Dims): GraphLayout {
  const first = depthsFor(index);
  const placed = placeDepths(index, first.depths, first.cyclic);
  const depths = placed.depths;

  const columns: NodeId[][] = [];
  for (const id of index.order) {
    const depth = depths.get(id) ?? 0;
    while (columns.length <= depth) columns.push([]);
    columns[depth]!.push(id);
  }

  const positions = new Map<NodeId, GraphPoint>();
  const rowIndex = new Map<NodeId, number>();
  const ordinal = new Map<NodeId, number>(index.order.map((id, position) => [id, position]));

  columns.forEach((rawColumn, depth) => {
    let column = [...rawColumn];
    if (depth > 0) {
      const orderValue = (id: NodeId): number => {
        const parents = (index.parents.get(id) ?? []).filter((parent) => rowIndex.has(parent));
        if (!parents.length) return Number.POSITIVE_INFINITY;
        let total = 0;
        for (const parent of parents) total += rowIndex.get(parent) ?? 0;
        const mean = total / parents.length;
        return isDecision(index, id) ? mean + 0.5 : mean;
      };
      const values = new Map(column.map((id) => [id, orderValue(id)]));
      column = stableSort(
        column,
        (id) => values.get(id) ?? Number.POSITIVE_INFINITY,
        (a, b) => (ordinal.get(a) ?? 0) - (ordinal.get(b) ?? 0),
      );
    }
    column.forEach((id, row) => {
      rowIndex.set(id, row);
      positions.set(id, { x: depth * dims.colW, y: row * dims.rowH, depth, row });
    });
  });

  const placedNodes: PlacedNode[] = index.order.map((id) => ({
    id,
    position: positions.get(id)!,
  }));

  let backwards = false;
  for (const edge of index.edges) {
    const parent = positions.get(edge.from);
    const child = positions.get(edge.to);
    if (!parent || !child) continue;
    if (child.depth <= parent.depth) backwards = true;
  }
  const links = reduceLinks(index, positions);
  const routed = routeEdges(links, positions, dims, (point) => ({ band: 0, col: point.depth }));

  let maxDepth = 0;
  for (const id of index.order) maxDepth = Math.max(maxDepth, positions.get(id)!.depth);
  let tallest = 0;
  for (const column of columns) tallest = Math.max(tallest, column.length);
  const diagnostic = placed.cyclic || backwards
    ? tx("gr.cycle")
    : null;

  return {
    width: maxDepth * dims.colW + dims.nodeW,
    height: (tallest - 1) * dims.rowH + dims.nodeH,
    positions,
    nodes: placedNodes,
    edges: routed.edges,
    markers: routed.markers,
    links,
    diagnostic,
    nodeWidth: dims.nodeW,
    nodeHeight: dims.nodeH,
  };
}

/**
 * The edges worth drawing: an edge implied by a longer path (a→c beside
 * a→b→c) adds no information and only crosses the plan. Tracing still uses
 * every edge of the DTO.
 */
function reduceLinks(index: GraphIndex, positions: ReadonlyMap<NodeId, GraphPoint>): Link[] {
  const reach = new Map<NodeId, Set<NodeId>>();
  const visiting = new Set<NodeId>();
  const descendants = (id: NodeId): Set<NodeId> => {
    const known = reach.get(id);
    if (known) return known;
    const found = new Set<NodeId>();
    if (visiting.has(id)) return found;
    visiting.add(id);
    for (const child of index.children.get(id) ?? []) {
      found.add(child);
      for (const below of descendants(child)) found.add(below);
    }
    visiting.delete(id);
    reach.set(id, found);
    return found;
  };
  const seen = new Set<string>();
  const links: Link[] = [];
  for (const edge of index.edges) {
    if (!positions.has(edge.from) || !positions.has(edge.to)) continue;
    const key = `${edge.from}\u0000${edge.to}`;
    if (seen.has(key)) continue;
    seen.add(key);
    const implied = (index.children.get(edge.from) ?? []).some(
      (child) => child !== edge.to && descendants(child).has(edge.to),
    );
    if (!implied) links.push({ from: edge.from, to: edge.to });
  }
  return links;
}

/** Horizontal room between parallel buses that share one gap. */
const BUS_STEP = 6;
/** Badge height; its width grows with the label. */
export const MARKER_H = 18;
/** How far a badge may reach past the outer cards: the graph's side padding. */
export const MARKER_REACH = 12;

/** Source numbers for an `in` badge; three or more in a row read as a range. */
export function markerLabel(numbers: readonly number[]): string {
  const sorted = [...new Set(numbers)].sort((a, b) => a - b);
  const parts: string[] = [];
  for (let i = 0; i < sorted.length; ) {
    let j = i;
    while (j + 1 < sorted.length && sorted[j + 1] === sorted[j]! + 1) j += 1;
    if (j - i >= 2) parts.push(`${sorted[i]}–${sorted[j]}`);
    else for (let k = i; k <= j; k += 1) parts.push(String(sorted[k]));
    i = j + 1;
  }
  return parts.join("·");
}

/**
 * Route the drawn edges. Neighbouring columns share one vertical bus per gap
 * (one per connected group, side by side when groups overlap), so a fan of
 * dependencies reads as a single line with branches. Every edge stays its own
 * path through that bus, for tracing and packets. An edge into another band or
 * past a column becomes a numbered marker pair on the card edges.
 */
function routeEdges(
  links: Link[],
  positions: ReadonlyMap<NodeId, GraphPoint>,
  dims: Dims,
  place: (point: GraphPoint) => { band: number; col: number },
): { edges: PlacedEdge[]; markers: GraphMarker[] } {
  const midY = (point: GraphPoint) => point.y + dims.nodeH / 2;
  const gap = dims.colW - dims.nodeW;
  const adjacent: Link[] = [];
  const skipping: Link[] = [];
  for (const link of links) {
    const from = place(positions.get(link.from)!);
    const to = place(positions.get(link.to)!);
    (from.band === to.band && to.col === from.col + 1 ? adjacent : skipping).push(link);
  }

  // One bus per connected group in each gap.
  const busX = new Map<Link, number>();
  const gutters = new Map<string, Link[]>();
  for (const link of adjacent) {
    const from = place(positions.get(link.from)!);
    const key = `${from.band}:${from.col}`;
    gutters.set(key, [...(gutters.get(key) ?? []), link]);
  }
  for (const group of gutters.values()) {
    const root = new Map<NodeId, NodeId>();
    const find = (id: NodeId): NodeId => {
      let at = id;
      while (root.get(at) !== undefined && root.get(at) !== at) at = root.get(at)!;
      return at;
    };
    for (const link of group) {
      for (const id of [link.from, link.to]) if (!root.has(id)) root.set(id, id);
      root.set(find(link.to), find(link.from));
    }
    const components = new Map<NodeId, { links: Link[]; top: number; bottom: number }>();
    for (const link of group) {
      const key = find(link.from);
      const ys = [midY(positions.get(link.from)!), midY(positions.get(link.to)!)];
      const entry = components.get(key) ?? { links: [], top: Infinity, bottom: -Infinity };
      entry.links.push(link);
      entry.top = Math.min(entry.top, ...ys);
      entry.bottom = Math.max(entry.bottom, ...ys);
      components.set(key, entry);
    }
    const first = positions.get(group[0]!.from)!;
    const centre = first.x + dims.nodeW + gap / 2;
    const reach = Math.max(0, Math.floor((gap / 2 - BUS_STEP) / BUS_STEP));
    const slots = [0];
    for (let step = 1; step <= reach; step += 1) slots.push(-step * BUS_STEP, step * BUS_STEP);
    const placed: { top: number; bottom: number; offset: number }[] = [];
    const ordered = [...components.values()].sort((a, b) => a.top - b.top || a.bottom - b.bottom);
    for (const component of ordered) {
      const taken = new Set(
        placed
          .filter((other) => other.top <= component.bottom && component.top <= other.bottom)
          .map((other) => other.offset),
      );
      const offset = slots.find((slot) => !taken.has(slot)) ?? 0;
      placed.push({ top: component.top, bottom: component.bottom, offset });
      for (const link of component.links) busX.set(link, centre + offset);
    }
  }

  const edges: PlacedEdge[] = adjacent.map((link) => {
    const from = positions.get(link.from)!;
    const to = positions.get(link.to)!;
    const x1 = from.x + dims.nodeW;
    const y1 = midY(from);
    const x2 = to.x - 3;
    const y2 = midY(to);
    const bx = busX.get(link)!;
    return { from: link.from, to: link.to, path: `M${x1} ${y1} H${bx} V${y2} H${x2}` };
  });

  // Marker pairs: one number per source, in reading order.
  const inset = Math.round(dims.nodeH / 4);
  const width = (label: string) => Math.max(MARKER_H + 2, 10 + label.length * 7);
  const sources = [...new Set(skipping.map((link) => link.from))].sort((a, b) => {
    const pa = positions.get(a)!;
    const pb = positions.get(b)!;
    const qa = place(pa);
    const qb = place(pb);
    return qa.band - qb.band || qa.col - qb.col || pa.y - pb.y;
  });
  const number = new Map(sources.map((id, i) => [id, String(i + 1)]));
  const markers: GraphMarker[] = [];
  for (const id of sources) {
    const point = positions.get(id)!;
    const label = number.get(id)!;
    markers.push({
      kind: "out",
      node: id,
      label,
      x: point.x + dims.nodeW,
      y: point.y + dims.nodeH - inset,
      width: width(label),
      edges: skipping.filter((link) => link.from === id),
    });
  }
  const targets = [...new Set(skipping.map((link) => link.to))];
  for (const id of targets) {
    const point = positions.get(id)!;
    const into = skipping.filter((link) => link.to === id);
    const label = markerLabel(into.map((link) => Number(number.get(link.from)!)));
    const y = point.y + dims.nodeH - inset;
    // A wide badge on a first-column card would reach past the side padding
    // and clip; it moves right just enough to stay whole.
    const x = Math.max(point.x, width(label) / 2 - MARKER_REACH);
    markers.push({ kind: "in", node: id, label, x, y, width: width(label), edges: into });
    for (const link of into) {
      // Packets for a marker edge play at the target's badge.
      edges.push({ from: link.from, to: link.to, path: `M${point.x - 10} ${y} H${point.x + 10}`, marker: true });
    }
  }
  return { edges, markers };
}

/** Vertical gap between wrapped bands of a long chain. */
export const BAND_GAP = 34;

/**
 * Wrap a layered chain that is wider than the column into bands, like lines
 * of text: columns keep their left-to-right order and continue on the next
 * band. Edges are routed afterwards: buses inside a band, numbered markers
 * into a later band. Nothing scrolls sideways and nothing is scaled.
 */
function wrapBands(layout: GraphLayout, available: number, dims: Dims): GraphLayout {
  let maxDepth = 0;
  for (const point of layout.positions.values()) maxDepth = Math.max(maxDepth, point.depth);
  const perBand = Math.max(1, Math.floor((available + (dims.colW - dims.nodeW)) / dims.colW));
  if (maxDepth + 1 <= perBand) return layout;

  const bands = Math.ceil((maxDepth + 1) / perBand);
  const rowsInBand = new Array<number>(bands).fill(0);
  for (const point of layout.positions.values()) {
    const band = Math.floor(point.depth / perBand);
    rowsInBand[band] = Math.max(rowsInBand[band]!, point.row + 1);
  }
  const bandTop: number[] = [];
  let y = 0;
  for (let band = 0; band < bands; band += 1) {
    bandTop.push(y);
    y += rowsInBand[band]! * dims.rowH - (dims.rowH - dims.nodeH) + BAND_GAP;
  }
  const positions = new Map<NodeId, GraphPoint>();
  for (const [id, point] of layout.positions) {
    const band = Math.floor(point.depth / perBand);
    positions.set(id, {
      ...point,
      x: (point.depth % perBand) * dims.colW,
      y: bandTop[band]! + point.row * dims.rowH,
    });
  }
  const routed = routeEdges(layout.links ?? [], positions, dims, (point) => ({
    band: Math.floor(point.depth / perBand),
    col: point.depth % perBand,
  }));
  return {
    ...layout,
    width: Math.min(maxDepth + 1, perBand) * dims.colW - (dims.colW - dims.nodeW),
    height: y - BAND_GAP,
    positions,
    nodes: layout.nodes.map(({ id }) => ({ id, position: positions.get(id)! })),
    edges: routed.edges,
    markers: routed.markers,
  };
}

/**
 * One milestone lane of the plan map. Steps that take part in a dependency
 * inside the lane are layered left to right; independent steps wrap into a
 * grid below them that uses the available width. Nothing is scaled down: a
 * lane wider than the column scrolls horizontally instead.
 *
 * `rank` orders the independent steps (lower first); ties keep DTO order.
 */
export function layoutLane(
  graph: Graph,
  available: number,
  rank: (id: NodeId) => number = () => 0,
): GraphLayout {
  const index = indexGraph(graph);
  if (!index.order.length) return emptyLayout();
  const dims: Dims = {
    colW: LANE_NODEW + LANE_GAP_X,
    rowH: LANE_NODEH + LANE_GAP_Y,
    nodeW: LANE_NODEW,
    nodeH: LANE_NODEH,
  };
  const linked = new Set<NodeId>();
  for (const edge of index.edges) {
    linked.add(edge.from);
    linked.add(edge.to);
  }
  const chained = linked.size
    ? wrapBands(
        layered(
          indexGraph({
            nodes: graph.nodes.filter((node) => linked.has(node.id)),
            edges: graph.edges,
          }),
          dims,
        ),
        available,
        dims,
      )
    : emptyLayout();

  const ordinal = new Map<NodeId, number>(index.order.map((id, position) => [id, position]));
  const free = stableSort(
    index.order.filter((id) => !linked.has(id)),
    rank,
    (a, b) => (ordinal.get(a) ?? 0) - (ordinal.get(b) ?? 0),
  );
  const positions = new Map(chained.positions);
  const top = linked.size ? chained.height + (free.length ? FREE_CAPTION + 8 : 0) : 0;
  const perRow = Math.max(1, Math.floor((available + FREE_GAP_X) / (LANE_NODEW + FREE_GAP_X)));
  free.forEach((id, i) => {
    const row = Math.floor(i / perRow);
    positions.set(id, {
      x: (i % perRow) * (LANE_NODEW + FREE_GAP_X),
      y: top + row * dims.rowH,
      depth: 0,
      row,
    });
  });
  const freeRows = Math.ceil(free.length / perRow);
  const freeWidth = free.length
    ? Math.min(free.length, perRow) * (LANE_NODEW + FREE_GAP_X) - FREE_GAP_X
    : 0;
  return {
    width: Math.max(chained.width, freeWidth),
    height: free.length ? top + freeRows * dims.rowH - LANE_GAP_Y : chained.height,
    positions,
    nodes: index.order.map((id) => ({ id, position: positions.get(id)! })),
    edges: chained.edges,
    markers: chained.markers,
    diagnostic: chained.diagnostic,
    nodeWidth: LANE_NODEW,
    nodeHeight: LANE_NODEH,
    freeTop: linked.size && free.length ? chained.height + 6 : null,
  };
}

/**
 * Vertical scale that keeps a graph inside the reading column without a
 * horizontal scrollbar. Four columns (724 px) fit the default window; a much
 * wider plan is scaled down, never silently clipped.
 */
export function fitScale(graphWidth: number, available: number): number {
  if (!(graphWidth > 0) || !(available > 0)) return 1;
  if (available >= graphWidth) return 1;
  return Math.round((available / graphWidth) * 1000) / 1000;
}
