/**
 * Project overview under the header: one discrete segment per step (never a
 * percentage bar), grouped by state, with a legend of counts. On a
 * long project, older finished steps fold into one bar so the recent, working
 * and planned squares stay readable.
 */

import { useRef, type CSSProperties } from "react";
import { tx, type MessageKey } from "../i18n";
import { formatFinishWindow, formatWorkRange } from "../api/format";
import type { FeatureState, ProjectForecast, ProjectState } from "../api/types";
import { forecastBasis, reviewWaiting } from "../state/project";
import { isRadarCandidate } from "../graph/display";
import { squareState } from "./PlanMap";
import { useElementWidth } from "../hooks/useElementWidth";

const ORDER = ["verified", "done", "review", "doing", "stopped", "you", "todo"] as const;
const LABEL: Record<(typeof ORDER)[number], MessageKey> = {
  verified: "ov.verified",
  done: "ov.done",
  review: "ov.review",
  doing: "ov.doing",
  stopped: "ov.stopped",
  you: "ov.you",
  todo: "ov.todo",
};

export function stepsByState(features: FeatureState[], waiting?: ReadonlySet<string>): FeatureState[] {
  const rank = (feature: FeatureState) => {
    const state = squareState(feature, waiting);
    return state === "dropped" ? ORDER.length : ORDER.indexOf(state);
  };
  return [...features]
    .filter((feature) => feature.progress !== "dropped")
    .sort((a, b) => rank(a) - rank(b) || a.title.localeCompare(b.title));
}

/** Squares narrower than this fold older finished steps. */
const SQUARE_MIN = 8;
const SQUARE_MAX = 12;
const GAP = 2;
/** Finished steps that stay squares when the rest fold. */
const KEEP_FINISHED = 6;

export interface OverviewBar {
  /** Older finished steps folded into one bar, oldest first; empty when nothing folds. */
  folded: FeatureState[];
  /** Steps drawn as squares, in order. */
  squares: FeatureState[];
  /** Square width in px while folded; unset lets the squares share the width. */
  square?: number;
}

/** Finished: verified or done with no review waiting for the owner. */
const isFinished = (feature: FeatureState, waiting?: ReadonlySet<string>) => {
  const state = squareState(feature, waiting);
  return state === "verified" || state === "done";
};

/** The step's latest event id; ids sort by time, so this orders finished steps. */
function lastTouched(feature: FeatureState): string {
  const attempt = feature.attempt ?? "";
  const pulse = feature.last_pulse ?? "";
  return attempt > pulse ? attempt : pulse;
}

/**
 * Folds only when a square would be narrower than SQUARE_MIN and more than
 * KEEP_FINISHED steps are finished. Then the folded bar comes first, the last
 * finished steps follow oldest to newest, then everything not finished.
 */
export function overviewBar(steps: FeatureState[], width: number, waiting?: ReadonlySet<string>): OverviewBar {
  const finishedOf = (feature: FeatureState) => isFinished(feature, waiting);
  const finished = steps.filter(finishedOf);
  const fits = (width + GAP) / steps.length - GAP >= SQUARE_MIN;
  if (fits || finished.length <= KEEP_FINISHED) return { folded: [], squares: steps };
  const byTime = [...finished].sort(
    (a, b) => lastTouched(a).localeCompare(lastTouched(b)) || a.title.localeCompare(b.title),
  );
  const squares = [...byTime.slice(-KEEP_FINISHED), ...steps.filter((feature) => !finishedOf(feature))];
  // The squares take at most four fifths of the row; the folded bar gets the rest.
  const square = Math.max(2, Math.min(SQUARE_MAX, (width * 0.8 + GAP) / squares.length - GAP));
  return { folded: byTime.slice(0, -KEEP_FINISHED), squares, square };
}

/** " · 2 in progress · 1 abandoned · 26 not started": the work left by who stands behind it. */
export function workSplit(forecast: ProjectForecast): string {
  const parts: string[] = [];
  if (forecast.in_progress) parts.push(tx("ov.split.doing", { n: forecast.in_progress }));
  if (forecast.abandoned) parts.push(tx("ov.split.abandoned", { n: forecast.abandoned }));
  if (forecast.not_started) parts.push(tx("ov.split.todo", { n: forecast.not_started }));
  return parts.map((part) => ` · ${part}`).join("");
}

export function ProjectOverview({ project, motion = false }: { project: ProjectState; motion?: boolean }) {
  const waiting = reviewWaiting(project);
  const steps = stepsByState(Object.values(project.features), waiting);
  if (!steps.length) return null;
  const counts = new Map<string, number>();
  for (const feature of steps) {
    const state = squareState(feature, waiting);
    counts.set(state, (counts.get(state) ?? 0) + 1);
  }
  // Waiting for your review counts pending reviews only; the rest is finished.
  const finished = (counts.get("verified") ?? 0) + (counts.get("done") ?? 0);
  const summary = tx("ov.summary", { f: finished, n: steps.length });
  return (
    <div className="overview">
      <Segments steps={steps} summary={summary} motion={motion} waiting={waiting} />
      <span className="olegend">
        {ORDER.filter((state) => counts.get(state)).map((state) => (
          <span key={state}>
            <i className={`osw ${state}`} aria-hidden="true" />
            <b>{counts.get(state)}</b> {tx(LABEL[state])}
          </span>
        ))}
        <span className="ototal">
          {summary}
          {project.stats?.forecast ? (
            <span className="oforecast" title={forecastBasis(project.stats.forecast)}>
              {project.stats.forecast.finish
                ? tx("ov.done_at", { when: formatFinishWindow(project.stats.forecast.finish) })
                : tx("ov.left", { work: formatWorkRange(project.stats.forecast.work_ms) })}
              {workSplit(project.stats.forecast)}
            </span>
          ) : null}
        </span>
      </span>
    </div>
  );
}

function Segments({
  steps,
  summary,
  motion,
  waiting,
}: {
  steps: FeatureState[];
  summary: string;
  motion: boolean;
  waiting: ReadonlySet<string>;
}) {
  const barRef = useRef<HTMLSpanElement>(null);
  const bar = overviewBar(steps, useElementWidth(barRef, 720), waiting);
  const folded = bar.folded.length;
  return (
    <span
      ref={barRef}
      className={folded ? "osegs folded" : "osegs"}
      role="img"
      aria-label={summary}
      style={bar.square ? ({ "--osq": `${bar.square}px` } as CSSProperties) : undefined}
    >
      {folded ? (
        <span className="ofold" title={folded === 1 ? tx("ov.folded.one") : tx("ov.folded.other", { n: folded })}>
          {(["verified", "done"] as const).map((state) => {
            const n = bar.folded.filter((feature) => squareState(feature, waiting) === state).length;
            return n ? <i key={state} className={state} style={{ flexGrow: n }} /> : null;
          })}
        </span>
      ) : null}
      {bar.squares.map((feature) => (
        <i
          key={feature.node}
          className={`${squareState(feature, waiting)}${motion && isRadarCandidate(feature) ? " live" : ""}`}
          title={feature.title}
        />
      ))}
    </span>
  );
}
