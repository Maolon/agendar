/**
 * The plan as a milestone roadmap: one lane per milestone on a vertical spine.
 * Finished lanes start collapsed into a row of status squares; open lanes draw
 * their part of the dependency graph at full size. Tracing uses the whole
 * plan, so selecting a step lights its upstream and downstream in every lane.
 */

import { correctionReason } from "../state/claim";
import { tx } from "../i18n";
import { useEffect, useMemo, useRef, useState } from "react";
import type { FeatureState, Graph as GraphDto, ProjectState } from "../api/types";
import { buildLanes, focusWindow, type Lane } from "../graph/lanes";
import { layoutLane } from "../graph/layout";
import type { MotionPulse } from "../graph/motion";
import { highlightSets, indexGraph } from "../graph/traversal";
import { Graph, type GraphPlay } from "./Graph";
import { Icon } from "./Icon";
import { useElementWidth } from "../hooks/useElementWidth";
import { reviewWaiting, waitsForReview } from "../state/project";
import { Untouched } from "./Untouched";

interface PlanMapProps {
  graph: GraphDto;
  /**
   * `graph` is the snapshot's outline (`planOutline`), not the answered graph:
   * lanes and counts show, and each open lane keeps its room empty.
   */
  outline?: boolean;
  project: ProjectState;
  selected: string | null;
  onSelect: (node: string | null) => void;
  onOpenNeed: (needId: string) => void;
  motion: boolean;
  pulses: MotionPulse[];
  pulseToken: number;
  play: GraphPlay | null;
  onPlayed: (answerId: string) => void;
  canMutate?: boolean;
  onRefresh?: (() => void) | undefined;
}

type SquareState = "verified" | "done" | "review" | "doing" | "you" | "stopped" | "todo" | "dropped";

/** `waiting`: steps with a review pending (`reviewWaiting`); without it a done step reads finished. */
export function squareState(feature: FeatureState, waiting?: ReadonlySet<string>): SquareState {
  if (feature.progress === "dropped") return "dropped";
  if (feature.phase === "in_review" || (waiting && waitsForReview(feature, waiting))) return "review";
  if (feature.progress === "verified") return "verified";
  if (feature.progress === "done") return "done";
  if (feature.gate === "awaiting_decision") return "you";
  // The claim decides when the core sends one: a claim that ended is stopped.
  if (feature.claim ? feature.claim.state === "free" : feature.liveness === "lost") return "stopped";
  if (feature.progress === "doing") return "doing";
  return "todo";
}

/** Independent steps: the moving and waiting ones first, finished, then dropped. */
function freeRank(project: ProjectState) {
  const order: Record<SquareState, number> = {
    you: 0,
    stopped: 1,
    doing: 2,
    todo: 3,
    review: 4,
    done: 5,
    verified: 5,
    dropped: 6,
  };
  const waiting = reviewWaiting(project);
  return (id: string): number => {
    const feature = id.startsWith("feature:") ? project.features[id.slice(8)] : undefined;
    return feature ? order[squareState(feature, waiting)] : 0;
  };
}

function laneStatus(lane: Lane): string {
  if (lane.complete) return tx("plan.all_done");
  const rest = lane.total - lane.finished - lane.active;
  return [lane.active ? tx("plan.in_progress", { n: lane.active }) : "", rest ? tx("plan.to_go", { n: rest }) : ""]
    .filter(Boolean)
    .join(" · ");
}

export function PlanMap({
  graph,
  outline = false,
  project,
  selected,
  onSelect,
  onOpenNeed,
  motion,
  pulses,
  pulseToken,
  play,
  onPlayed,
  canMutate = false,
  onRefresh,
}: PlanMapProps) {
  const whole = useMemo(() => buildLanes(graph, project.features), [graph, project.features]);
  /** Untouched steps leave their lane's drawing for the folded last lane; lane counts keep them. */
  const untouched = useMemo(
    () => whole.lanes.flatMap((lane) => lane.features.filter((feature) => feature.untouched)),
    [whole.lanes],
  );
  const plan = useMemo(() => {
    if (!untouched.length) return whole;
    const gone = new Set(untouched.map((feature) => `feature:${feature.node}`));
    return {
      ...whole,
      lanes: whole.lanes.map((lane) => ({
        ...lane,
        graph: {
          nodes: lane.graph.nodes.filter((node) => !gone.has(node.id)),
          edges: lane.graph.edges.filter((edge) => !gone.has(edge.from) && !gone.has(edge.to)),
        },
      })),
    };
  }, [untouched, whole]);
  const traced = useMemo(() => {
    if (!selected) return null;
    return highlightSets(indexGraph(graph), selected).nodes;
  }, [graph, selected]);
  const [toggled, setToggled] = useState<Record<string, boolean>>({});
  /** Lanes showing every step instead of the window around current work. */
  const [showAll, setShowAll] = useState<Record<string, boolean>>({});
  const windows = useMemo(
    () => new Map(plan.lanes.map((lane) => [lane.key, focusWindow(lane, project.features)])),
    [plan.lanes, project.features],
  );
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const width = useElementWidth(wrapRef, 720);
  const rank = useMemo(() => freeRank(project), [project]);
  const waiting = useMemo(() => reviewWaiting(project), [project]);

  /** A lane with nothing to draw (all its open steps are untouched) stays a row of squares. */
  const isOpen = (lane: Lane): boolean => lane.graph.nodes.length > 0 && (toggled[lane.key] ?? !lane.complete);
  const selectedLane = selected ? plan.laneOf.get(selected) : undefined;
  const playLane = play ? plan.laneOf.get(play.from) : undefined;

  // A selection or an unblock moment in a collapsed lane opens that lane.
  useEffect(() => {
    for (const key of [selectedLane, playLane]) {
      if (key === undefined) continue;
      const lane = plan.lanes.find((item) => item.key === key);
      if (lane && lane.graph.nodes.some((node) => node.id === selected || node.id === play?.from) && !(toggled[lane.key] ?? !lane.complete)) {
        setToggled((current) => ({ ...current, [key]: true }));
      }
    }
  }, [plan.lanes, playLane, selectedLane, toggled]);

  // A selection hidden by a lane's window shows that whole lane.
  useEffect(() => {
    if (!selectedLane || showAll[selectedLane] || !selected) return;
    // An untouched step is drawn in the last lane, not in its milestone.
    if (!plan.lanes.some((lane) => lane.graph.nodes.some((node) => node.id === selected))) return;
    const window = windows.get(selectedLane);
    if (window && !window.graph.nodes.some((node) => node.id === selected)) {
      setShowAll((current) => ({ ...current, [selectedLane]: true }));
    }
  }, [plan.lanes, selected, selectedLane, showAll, windows]);

  // An unblock whose source is not drawn (an answered decision) has nothing to
  // animate; mark it played so it is never retried.
  // The outline has no decision nodes: only the answered graph can tell.
  useEffect(() => {
    if (!outline && play && !play.played && playLane === undefined) onPlayed(play.answerId);
  }, [onPlayed, outline, play, playLane]);

  if (!plan.lanes.length) return <div className="muted-note">{tx("plan.none")}</div>;

  return (
    <div className="pmap" ref={wrapRef} aria-busy={outline || undefined}>
      {plan.lanes.map((lane) => {
        const open = isOpen(lane);
        const state = lane.complete ? "verified" : lane.active || lane.finished ? "doing" : "todo";
        return (
          <section className="plane" key={lane.key} data-lane={lane.key} aria-label={lane.label}>
            <span className={`plane-dot gd ${state}`} aria-hidden="true">
              {lane.complete ? <Icon name="check" /> : null}
            </span>
            <button
              type="button"
              className="plane-h"
              aria-expanded={open}
              onClick={() => setToggled((current) => ({ ...current, [lane.key]: !open }))}
            >
              <Icon name="chevron-right" className="plane-chev" />
              <span className="plane-name">{lane.label}</span>
              <span className="plane-count">
                {lane.finished}/{lane.total}
              </span>
              <span className="plane-status">{laneStatus(lane)}</span>
            </button>
            {open ? (
              <div className="plane-body">
                {(() => {
                  const window = windows.get(lane.key)!;
                  const windowed =
                    window.hiddenBefore > 0 || window.hiddenAfter > 0 || window.hiddenFinished > 0;
                  if (!windowed) return null;
                  const all = showAll[lane.key] === true;
                  const plural = (n: number) => (n === 1 ? tx("plan.steps.one") : tx("plan.steps.other", { n }));
                  return (
                    <div className="plane-window">
                      {all ? (
                        <button
                          type="button"
                          className="link"
                          onClick={() => setShowAll((current) => ({ ...current, [lane.key]: false }))}
                        >
                          {window.hiddenFinished && !window.hiddenBefore && !window.hiddenAfter
                            ? tx("plan.hide_finished")
                            : tx("plan.focus")}
                        </button>
                      ) : (
                        <>
                          {window.hiddenBefore ? (
                            <button
                              type="button"
                              className="link"
                              onClick={() => setShowAll((current) => ({ ...current, [lane.key]: true }))}
                            >
                              {tx("plan.show_before", { what: plural(window.hiddenBefore) })}
                            </button>
                          ) : null}
                          {window.hiddenAfter ? (
                            <button
                              type="button"
                              className="link"
                              onClick={() => setShowAll((current) => ({ ...current, [lane.key]: true }))}
                            >
                              {tx("plan.show_after", { what: plural(window.hiddenAfter) })}
                            </button>
                          ) : null}
                          {window.hiddenFinished ? (
                            <button
                              type="button"
                              className="link"
                              onClick={() => setShowAll((current) => ({ ...current, [lane.key]: true }))}
                            >
                              {tx("plan.show_finished", { n: window.hiddenFinished })}
                            </button>
                          ) : null}
                        </>
                      )}
                    </div>
                  );
                })()}
                {(() => {
                  const window = windows.get(lane.key)!;
                  const shown = showAll[lane.key] ? lane.graph : window.graph;
                  const layout = layoutLane(shown, Math.max(240, width - 40), rank);
                  if (outline) {
                    // The room the lane's drawing takes, kept until the graph answers.
                    return (
                      <div className="gwrap" aria-hidden="true">
                        <div className="gstage" style={{ width: layout.width, height: layout.height }} />
                      </div>
                    );
                  }
                  const after =
                    !showAll[lane.key] && window.after.size
                      ? new Map([...plan.after, ...window.after])
                      : plan.after;
                  return (
                    <Graph
                      graph={shown}
                      layout={layout}
                      after={after}
                      traceGraph={graph}
                      project={project}
                      selected={selected}
                      onSelect={onSelect}
                      onOpenNeed={onOpenNeed}
                      motion={motion}
                      pulses={pulses}
                      pulseToken={pulseToken}
                      play={playLane === lane.key ? play : null}
                      onPlayed={onPlayed}
                    />
                  );
                })()}
              </div>
            ) : (
              <div className="plane-squares" aria-hidden="true">
                {lane.features.map((feature) => (
                  <i
                    key={feature.node}
                    className={`${squareState(feature, waiting)}${
                      traced && !traced.has(`feature:${feature.node}`) ? " dim" : ""
                    }`}
                    title={feature.correction ? `${feature.title} · ${correctionReason(feature.correction)}` : feature.title}
                  />
                ))}
              </div>
            )}
          </section>
        );
      })}
      <Untouched
        project={project}
        steps={untouched}
        selected={selected}
        onSelect={onSelect}
        canMutate={canMutate}
        onRefresh={onRefresh}
      />
    </div>
  );
}
