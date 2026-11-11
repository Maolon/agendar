/**
 * The demo budget, said in full: how long work has been in flight with no
 * delivered demo, which step that is, and what clears it. The clock itself is
 * the core's (`summary.budget.demo_gap_s`), and so is the step it runs for
 * (`summary.budget.demo_gap_node`).
 */

import { tx } from "../i18n";
import type { FeatureState, ProjectState } from "../api/types";
import { lateText } from "./now";
import { reviewWaiting } from "./project";

/** What an agent runs to deliver a demo. */
export const DELIVER_COMMAND = "agendar deliver";

export interface DemoGap {
  gapS: number;
  limitS: number;
  /** At or past the limit: the project shows "No demo". */
  over: boolean;
  /** The step the clock runs for, as the core names it, then the others like it by title. */
  steps: FeatureState[];
  /** Nothing is in progress: the steps are delivered and wait for a review. */
  review: boolean;
}

/** Null while no work is in flight (the clock reads zero). */
export function demoGap(project: ProjectState): DemoGap | null {
  const gapS = project.summary.budget.demo_gap_s;
  const limitS = project.policy.value.budget.max_demo_gap_s;
  if (gapS <= 0 || limitS <= 0) return null;
  const features = Object.values(project.features);
  const byTitle = (list: FeatureState[]) => [...list].sort((a, b) => a.title.localeCompare(b.title));
  const doing = features.filter((feature) => feature.progress === "doing");
  const node = project.summary.budget.demo_gap_node;
  const named = node ? project.features[node] : undefined;
  const review = !named && doing.length === 0;
  let steps: FeatureState[] = [];
  if (named) {
    steps = [named, ...byTitle(doing.filter((feature) => feature !== named))];
  } else if (review) {
    const waiting = reviewWaiting(project);
    steps = byTitle(features.filter((feature) => feature.progress === "done" && waiting.has(feature.node)));
  }
  return { gapS, limitS, over: gapS >= limitS, steps, review };
}

/** "No demo for 5h (limit 4h)". */
export function demoGapWhat(gap: DemoGap): string {
  return tx("gap.what", { d: lateText(gap.gapS), limit: lateText(gap.limitS) });
}

/** A step title short enough for one line. */
const TITLE_MAX = 40;

function shortTitle(title: string): string {
  const chars = Array.from(title);
  return chars.length <= TITLE_MAX ? title : `${chars.slice(0, TITLE_MAX - 1).join("").trimEnd()}…`;
}

/** "Search index is started, not delivered"; empty when the board names no step. */
export function demoGapWho(gap: DemoGap): string {
  const first = gap.steps[0];
  if (!first) return "";
  const more = gap.steps.length - 1;
  const kind = gap.review ? "review" : "doing";
  const x = shortTitle(first.title);
  return more > 0 ? tx(`gap.${kind}.other`, { x, n: more }) : tx(`gap.${kind}.one`, { x });
}

/** "Clears when the agent runs agendar deliver." */
export function demoGapClear(gap: DemoGap): string {
  return gap.review ? tx("gap.clear.review") : tx("gap.clear.deliver", { cmd: DELIVER_COMMAND });
}

/** The whole line, for a tooltip or a description. */
export function demoGapLine(gap: DemoGap): string {
  const who = demoGapWho(gap);
  const clear = demoGapClear(gap);
  return who ? tx("gap.line", { what: demoGapWhat(gap), who, clear }) : `${demoGapWhat(gap)}. ${clear}`;
}
