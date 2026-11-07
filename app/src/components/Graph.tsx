/**
 * Dependency graph: the prototype's layered plan with the
 * frontend-owned geometry, node state, ancestry highlight and event-driven
 * motion (blips, packets, the unblock moment).
 *
 * Nothing here mutates the DTO; positions live in `graph/layout.ts`, ancestry in
 * `graph/traversal.ts`, and every animation is one committed pulse or one
 * persisted answer. Without motion the same information is rendered statically.
 */

import { tx } from "../i18n";
import { useEffect, useMemo, useRef, useState } from "react";
import type { Graph as GraphDto, ProjectState } from "../api/types";
import { isRadarCandidate, nodeDisplay } from "../graph/display";
import { layoutGraph, MARKER_H, NODEH, NODEW, type GraphLayout } from "../graph/layout";
import {
  BLIP_REMOVE_MS,
  PACKET_REMOVE_MS,
  UNBLOCK_BLIP_MS,
  UNBLOCK_PACKET_MS,
  pulsePacketPlan,
  unblockPacketPlan,
  type MotionPulse,
} from "../graph/motion";
import { highlightSets, indexGraph, isHighlighted, nodeLabel } from "../graph/traversal";
import { Icon } from "./Icon";

export interface GraphPlay {
  answerId: string;
  from: string;
  released: string[];
  played: boolean;
}

interface GraphProps {
  graph: GraphDto;
  project: ProjectState;
  selected: string | null;
  onSelect: (node: string | null) => void;
  /** Pending decisions/reviews open their card through this explicit action. */
  onOpenNeed: (needId: string) => void;
  motion: boolean;
  pulses: MotionPulse[];
  pulseToken: number;
  play: GraphPlay | null;
  onPlayed: (answerId: string) => void;
  /** Precomputed geometry (a plan-map lane); defaults to the classic layout. */
  layout?: GraphLayout;
  /** Full plan used for tracing, so a lane highlights across lanes. */
  traceGraph?: GraphDto;
  /** Node id -> what it waits on in other lanes ("after …"). */
  after?: ReadonlyMap<string, string>;
}

interface Bubble {
  key: string;
  node?: string;
  path?: string;
  from?: string;
  to?: string;
}

/**
 * One travelling packet. SMIL `begin="0s"` means SVG document time zero, so a
 * packet inserted into a graph that has been on screen for a while would sit
 * frozen at its destination. `begin="indefinite"` plus `beginElement()` in the
 * mount effect starts every packet at its own insertion time.
 */
function Packet({ bubble }: { bubble: Bubble }) {
  const animation = useRef<SVGElement | null>(null);

  useEffect(() => {
    const element = animation.current as
      | (SVGElement & { beginElement?: () => void })
      | null;
    if (!element || typeof element.beginElement !== "function") return;
    try {
      element.beginElement();
    } catch {
      // Without SMIL support the packet stays on its static path.
    }
  }, [bubble.path]);

  return (
    <circle className="pk" r={3.2} data-packet={`${bubble.from}->${bubble.to}`}>
      <animateMotion
        ref={animation}
        dur="1.1s"
        fill="freeze"
        begin="indefinite"
        path={bubble.path}
        calcMode="spline"
        keyTimes="0;1"
        keySplines=".45 0 .25 1"
      />
    </circle>
  );
}

/** DOM id for one node, for the In-flight list to scroll into view. */
export function graphNodeElementId(id: string): string {
  return `gnode-${id.replace(/[^A-Za-z0-9_-]/g, "_")}`;
}

/**
 * One graph. Selecting a node traces ancestors + descendants; pending
 * decision/review nodes additionally open their card through a labelled
 * secondary control (single click keeps tracing possible).
 */
export function Graph({
  graph,
  project,
  selected,
  onSelect,
  onOpenNeed,
  motion,
  pulses,
  pulseToken,
  play,
  onPlayed,
  layout: given,
  traceGraph,
  after,
}: GraphProps) {
  const layout = useMemo(() => given ?? layoutGraph(graph), [given, graph]);
  const index = useMemo(() => indexGraph(graph), [graph]);
  const traceIndex = useMemo(
    () => (traceGraph ? indexGraph(traceGraph) : index),
    [index, traceGraph],
  );
  const highlight = useMemo(() => highlightSets(traceIndex, selected), [traceIndex, selected]);
  const nodeWidth = layout.nodeWidth ?? NODEW;
  const nodeHeight = layout.nodeHeight ?? NODEH;
  const [bubbles, setBubbles] = useState<Bubble[]>([]);
  const timers = useRef<number[]>([]);
  const lastToken = useRef(pulseToken);

  // Clear every scheduled animation on unmount or when motion is switched off.
  useEffect(() => {
    if (motion) return;
    for (const timer of timers.current) window.clearTimeout(timer);
    timers.current = [];
    setBubbles([]);
  }, [motion]);

  useEffect(() => {
    return () => {
      for (const timer of timers.current) window.clearTimeout(timer);
      timers.current = [];
    };
  }, []);

  function schedule(delay: number, add: () => Bubble, removeAfter: number): void {
    const bubble = add();
    const key = bubble.key;
    if (delay > 0) {
      timers.current.push(
        window.setTimeout(() => {
          setBubbles((current) => [...current.filter((item) => item.key !== key), bubble]);
        }, delay),
      );
    } else {
      setBubbles((current) => [...current, bubble]);
    }
    timers.current.push(
      window.setTimeout(() => {
        setBubbles((current) => current.filter((item) => item.key !== key));
      }, delay + removeAfter),
    );
  }

  /** One blip per new pulse on an eligible node, plus a packet per outgoing edge. */
  useEffect(() => {
    if (!motion) return;
    if (pulseToken === lastToken.current) return;
    lastToken.current = pulseToken;
    const paths = new Map(layout.edges.map((edge) => [`${edge.from}->${edge.to}`, edge.path]));
    for (const pulse of pulses) {
      if (pulse.project !== project.summary.slug) continue;
      const id = `feature:${pulse.node}`;
      const position = layout.positions.get(id);
      if (!position) continue;
      schedule(0, () => ({ key: `blip-${pulse.id}`, node: id }), BLIP_REMOVE_MS);
      const outgoing = index.children.get(id) ?? [];
      const plans = pulsePacketPlan(id, outgoing, (to) => paths.get(`${id}->${to}`));
      for (const plan of plans) {
        schedule(
          plan.delay,
          () => ({ key: `packet-${pulse.id}-${plan.to}`, path: plan.path, from: plan.from, to: plan.to }),
          PACKET_REMOVE_MS,
        );
      }
    }
  }, [index, layout, motion, project.summary.slug, pulseToken, pulses]);

  /** The unblock moment: packet at 350 ms, released blips at 1050 ms. */
  useEffect(() => {
    if (!play || play.played) return;
    onPlayed(play.answerId);
    if (!motion) return;
    // `released` carries feature node paths; the drawn edges use graph node ids.
    const releasedIds = play.released.map((node) =>
      node.startsWith("feature:") ? node : `feature:${node}`,
    );
    const plans = unblockPacketPlan(layout.edges, play.from, releasedIds);
    for (const plan of plans) {
      schedule(
        UNBLOCK_PACKET_MS + plan.delay,
        () => ({ key: `unblock-packet-${plan.to}`, path: plan.path, from: plan.from, to: plan.to }),
        PACKET_REMOVE_MS,
      );
    }
    for (const node of releasedIds) {
      schedule(
        UNBLOCK_BLIP_MS,
        () => ({ key: `unblock-blip-${node}`, node }),
        BLIP_REMOVE_MS,
      );
    }
  }, [layout, motion, onPlayed, play]);

  if (!layout.nodes.length) {
    return <div className="muted-note">{tx("plan.none")}</div>;
  }

  // Never scaled down: a plan wider than the column scrolls horizontally, so
  // titles stay readable at any size.
  return (
    <div className="gwrap">
      {layout.diagnostic ? (
        <p className="muted-note" role="status">
          {layout.diagnostic}
        </p>
      ) : null}
      <div className="gstage" style={{ width: layout.width, height: layout.height }}>
        <div className="graph" style={{ width: layout.width, height: layout.height }}>
          {layout.freeTop != null ? (
            <div className="gfree" style={{ top: layout.freeTop }}>
              {tx("plan.independent")}
            </div>
          ) : null}
          <svg className="edges" width={layout.width} height={layout.height} aria-hidden="true">
            <defs>
              <marker
                id="garw"
                viewBox="0 0 10 10"
                refX="8"
                refY="5"
                markerWidth="6"
                markerHeight="6"
                orient="auto"
              >
                <path d="M0 1 L9 5 L0 9 z" className="mk" />
              </marker>
              <marker
                id="garw-hl"
                viewBox="0 0 10 10"
                refX="8"
                refY="5"
                markerWidth="6"
                markerHeight="6"
                orient="auto"
              >
                <path d="M0 1 L9 5 L0 9 z" className="mk-hl" />
              </marker>
            </defs>
            {/* Lit edges last, so a traced path is never under a shared bus. */}
            {[...layout.edges]
              .filter((edge) => !edge.marker)
              .sort((a, b) => Number(isHighlighted(highlight, a)) - Number(isHighlighted(highlight, b)))
              .map((edge) => {
              const lit = isHighlighted(highlight, edge);
              const className = `ge${selected ? (lit ? " hl" : " dim") : ""}`;
              return (
                <path
                  key={`${edge.from}->${edge.to}`}
                  className={className}
                  data-from={edge.from}
                  data-to={edge.to}
                  d={edge.path}
                  markerEnd={`url(#${lit ? "garw-hl" : "garw"})`}
                />
              );
            })}
            {bubbles
              .filter((bubble) => bubble.path)
              .map((bubble) => (
                <Packet key={bubble.key} bubble={bubble} />
              ))}
          </svg>
          {layout.nodes.map(({ id, position }) => {
            const node = index.nodes.get(id)!;
            const feature = node.feature ? project.features[node.feature] : undefined;
            const display = nodeDisplay(node, feature);
            const assumed = display.assumed;
            const dim = selected ? !highlight.nodes.has(id) : false;
            const eligible = isRadarCandidate(feature);
            const blips = bubbles.filter((bubble) => bubble.node === id);
            const openable = display.you ? node.need_id : undefined;
            return (
              <div
                key={id}
                id={graphNodeElementId(id)}
                className={[
                  "gnode",
                  display.you ? "you" : "",
                  assumed ? "assumed" : "",
                  feature?.basis === "tainted" ? "tainted" : "",
                  selected === id ? "selected" : "",
                  dim ? "dim" : "",
                ]
                  .filter(Boolean)
                  .join(" ")}
                style={{ left: position.x, top: position.y, width: nodeWidth, height: nodeHeight }}
                data-graph-node={id}
                data-depth={position.depth}
              >
                <button
                  type="button"
                  className="gnode-main"
                  aria-pressed={selected === id}
                  aria-label={`${display.label}. ${display.sub}. ${
                    selected === id ? tx("gr.stop") : tx("gr.trace")
                  }`}
                  title={display.title}
                  onClick={() => onSelect(selected === id ? null : id)}
                  onDoubleClick={() => {
                    if (openable) onOpenNeed(openable);
                  }}
                >
                  <span
                    className={`gd ${display.dot}${eligible && motion ? " live" : ""}`}
                    aria-hidden="true"
                  >
                    {display.icon ? <Icon name={display.icon} /> : null}
                    {blips.map((bubble) => (
                      <span key={bubble.key} className="blip" data-blip={id} />
                    ))}
                  </span>
                  <span className="gtext">
                    <span className="gn">{display.label}</span>
                    <span className="gs">
                      {after?.has(id) && display.dot !== "review" && display.dot !== "verified" ? (
                        <>
                          <Icon name="corner-down-left" className="gs-after" />
                          {tx("gr.after", { x: after.get(id) ?? "" })}
                        </>
                      ) : (
                        display.sub
                      )}
                    </span>
                  </span>
                </button>
                {openable ? (
                  <button
                    type="button"
                    className="gnode-open"
                    aria-label={tx("tray.open_x", { x: display.label })}
                    onClick={() => onOpenNeed(openable)}
                  >
                    <Icon name="chevron-right" />
                  </button>
                ) : null}
              </div>
            );
          })}
          {layout.markers.map((marker) => {
            const lit = marker.edges.some((edge) => isHighlighted(highlight, edge));
            const others = marker.edges.map((edge) =>
              nodeLabel(index, project, marker.kind === "out" ? edge.to : edge.from),
            );
            const title =
              marker.kind === "out"
                ? tx("gr.marker.out", { list: others.join(", ") })
                : tx("gr.marker.in", { list: others.join(", ") });
            return (
              <span
                key={`${marker.kind}:${marker.node}`}
                className={`gmark${selected ? (lit ? " hl" : " dim") : ""}`}
                style={{
                  left: marker.x - marker.width / 2,
                  top: marker.y - MARKER_H / 2,
                  width: marker.width,
                  height: MARKER_H,
                }}
                title={title}
                data-marker={marker.kind}
              >
                {marker.label}
              </span>
            );
          })}
        </div>
      </div>
    </div>
  );
}
