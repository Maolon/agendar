/**
 * The plan as a milestone roadmap: one lane per milestone on a vertical spine.
 * Finished lanes start collapsed into a row of status squares; open lanes draw
 * their part of the dependency graph at full size. Tracing uses the whole
 * plan, so selecting a step lights its upstream and downstream in every lane.
 */

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

interface PlanMapProps {
  graph: GraphDto;
  project: ProjectState;
  selected: string | null;
  onSelect: (node: string | null) => void;
  onOpenNeed: (needId: string) => void;
  motion: boolean;
  pulses: MotionPulse[];
  pulseToken: number;
  play: GraphPlay | null;
  onPlayed: (answerId: string) => void;
}

type SquareState = "verified" | "review" | "doing" | "you" | "stopped" | "todo" | "dropped";

export function squareState(feature: FeatureState): SquareState {
  if (feature.progress === "dropped") return "dropped";
  if (feature.progress === "verified") return "verified";
  if (feature.progress === "done") return "review";
  if (feature.gate === "awaiting_decision") return "you";
  if (feature.liveness === "lost") return "stopped";
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
    verified: 5,
    dropped: 6,
  };
  return (id: string): number => {
    const feature = id.startsWith("feature:") ? project.features[id.slice(8)] : undefined;
    return feature ? order[squareState(feature)] : 0;
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
  project,
  selected,
  onSelect,
  onOpenNeed,
  motion,
  pulses,
  pulseToken,
  play,
  onPlayed,
}: PlanMapProps) {
  const plan = useMemo(() => buildLanes(graph, project.features), [graph, project.features]);
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

  const isOpen = (lane: Lane): boolean => toggled[lane.key] ?? !lane.complete;
  const selectedLane = selected ? plan.laneOf.get(selected) : undefined;
  const playLane = play ? plan.laneOf.get(play.from) : undefined;

  // A selection or an unblock moment in a collapsed lane opens that lane.
  useEffect(() => {
    for (const key of [selectedLane, playLane]) {
      if (key === undefined) continue;
      const lane = plan.lanes.find((item) => item.key === key);
      if (lane && !(toggled[lane.key] ?? !lane.complete)) {
        setToggled((current) => ({ ...current, [key]: true }));
      }
    }
  }, [plan.lanes, playLane, selectedLane, toggled]);

  // A selection hidden by a lane's window shows that whole lane.
  useEffect(() => {
    if (!selectedLane || showAll[selectedLane] || !selected) return;
    const window = windows.get(selectedLane);
    if (window && !window.graph.nodes.some((node) => node.id === selected)) {
      setShowAll((current) => ({ ...current, [selectedLane]: true }));
    }
  }, [selected, selectedLane, showAll, windows]);

  // An unblock whose source is not drawn (an answered decision) has nothing to
  // animate; mark it played so it is never retried.
  useEffect(() => {
    if (play && !play.played && playLane === undefined) onPlayed(play.answerId);
  }, [onPlayed, play, playLane]);

  if (!plan.lanes.length) return <div className="muted-note">{tx("plan.none")}</div>;

  return (
    <div className="pmap" ref={wrapRef}>
      {plan.lanes.map((lane) => {
        const open = isOpen(lane);
        const state = lane.complete ? "review" : lane.active || lane.finished ? "doing" : "todo";
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
                <Graph
                  {...(() => {
                    const window = windows.get(lane.key)!;
                    const shown = showAll[lane.key] ? lane.graph : window.graph;
                    const after =
                      !showAll[lane.key] && window.after.size
                        ? new Map([...plan.after, ...window.after])
                        : plan.after;
                    return {
                      graph: shown,
                      layout: layoutLane(shown, Math.max(240, width - 40), rank),
                      after,
                    };
                  })()}
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
              </div>
            ) : (
              <div className="plane-squares" aria-hidden="true">
                {lane.features.map((feature) => (
                  <i
                    key={feature.node}
                    className={`${squareState(feature)}${
                      traced && !traced.has(`feature:${feature.node}`) ? " dim" : ""
                    }`}
                    title={feature.title}
                  />
                ))}
              </div>
            )}
          </section>
        );
      })}
    </div>
  );
}
